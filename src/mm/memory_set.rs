//! Implementation of [`MapArea`] and [`MemorySet`].

use crate::guest::page_table::GuestPageTable;
use crate::hyp_alloc::{ FrameTracker, frame_alloc };
use crate::page_table::{PTEFlags, PageTable};
use crate::page_table::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use crate::page_table::{StepByOne, VPNRange, PPNRange};
use crate::constants::{
    PAGE_SIZE,
    layout::{ TRAMPOLINE, TRAP_CONTEXT, MEMORY_END, GUEST_START_PA, GUEST_START_VA }
};
use crate::hypervisor::{ fdt::MachineMeta, HOST_VMM };
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use super::MemorySet;
use core::marker::PhantomData;
use core::arch::asm;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn strampoline();
    fn sinitrd();
    fn einitrd();
}




/// memory set structure, controls virtual-memory space
pub struct HostMemorySet<P: PageTable> {
    pub page_table: P,
    pub areas: Vec<MapArea<P>>,
}

pub struct GuestMemorySet<G: GuestPageTable> {
    pub page_table: G,
    pub areas: Vec<MapArea<G>>,
}

impl<P: PageTable> HostMemorySet<P> {
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
        }
    }

    /// Without kernel stacks.
    /// 内核虚拟地址映射
    /// 映射了内核代码段和数据段以及跳板页，没有映射内核栈
    pub fn new_host_vmm(machine: &MachineMeta) -> Self {
        let mut hpm = Self::new_bare();
        // map trampoline
        hpm.map_trampoline();

        // 这里注意了,需要单独映射 Trap Context,因为在上下文切换时
        // 我们是不切换页表的
        hpm.push(
            MapArea::new(
                TRAP_CONTEXT.into(),
                TRAMPOLINE.into(),
                None,
                None,
                MapType::Framed,
                MapPermission::R | MapPermission::W
            ),
            None,
        );

        // map kernel sections
        hpm.push(
            MapArea::new(
                (stext as usize).into(),
                (etext as usize).into(),
                Some((stext as usize).into()),
                Some((etext as usize).into()),
                MapType::Linear,
                MapPermission::R | MapPermission::X,
            ),
            None,
        );

        hpm.push(
            MapArea::new(
                (srodata as usize).into(),
                (erodata as usize).into(),
                Some((srodata as usize).into()),
                Some((erodata as usize).into()),
                MapType::Linear,
                MapPermission::R,
            ),
            None,
        );

        hpm.push(
            MapArea::new(
                (sdata as usize).into(),
                (edata as usize).into(),
                Some((sdata as usize).into()),
                Some((edata as usize).into()),
                MapType::Linear,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );

        hpm.push(
            MapArea::new(
                (sbss_with_stack as usize).into(),
                (ebss as usize).into(),
                Some((sbss_with_stack as usize).into()),
                Some((ebss as usize).into()),
                MapType::Linear,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );

        hpm.push(
            MapArea::new(
                (ekernel as usize).into(),
                MEMORY_END.into(),
                Some((ekernel as usize).into()),
                Some(MEMORY_END.into()),
                MapType::Linear,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );

        if let Some(test) = &machine.test_finisher_address {
            hpm.push(
                MapArea::new(
                    test.base_address.into(),
                    (test.base_address + test.size).into(),
                    Some(test.base_address.into()),
                    Some((test.base_address + test.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W,
                ), 
                None
            );
        }

        for virtio_dev in machine.virtio.iter() {
            hpm.push(
                MapArea::new(
                    virtio_dev.base_address.into(),
                    (virtio_dev.base_address + virtio_dev.size).into(),
                    Some(virtio_dev.base_address.into()),
                    Some((virtio_dev.base_address + virtio_dev.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W,
                ),
                None,
            )
        }

        if let Some(plic) = &machine.plic {
            hpm.push(
                MapArea::new(
                    plic.base_address.into(),
                    (plic.base_address + plic.size).into(),
                    Some(plic.base_address.into()),
                    Some((plic.base_address + plic.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W,
                ), 
                None
            );
        }
        hpm
    }

    /// 激活根页表
    pub fn activate(&self) {
        let satp = self.page_table.token();
        unsafe {
            asm!(
                "csrw satp, {hgatp}",
                "sfence.vma",
                hgatp = in(reg) satp
            );
        }
    }

    pub fn map_guest(&mut self, start_pa: usize, gpm_size: usize) {
        self.push(
            MapArea::new(
                start_pa.into(), 
                (start_pa + gpm_size).into(), 
                Some(start_pa.into()), 
                Some((start_pa + gpm_size).into()), 
                MapType::Linear, 
                MapPermission::R | MapPermission::W
            ), 
            None
        );
    }

    /// 加载客户操作系统
    pub fn map_gpm(&mut self, gpm: &GuestMemorySet<impl GuestPageTable>) {
        for area in gpm.areas.iter() {
            // 修改虚拟地址与物理地址相同
            let ppn_range = area.ppn_range.unwrap();
            let start_pa: PhysAddr = ppn_range.get_start().into();
            let end_pa: PhysAddr = ppn_range.get_end().into();
            let start_va: usize = start_pa.into();
            let end_va: usize= end_pa.into();
            let new_area = MapArea::new(
                start_va.into(), 
                end_va.into(), 
                Some(start_pa),
                Some(end_pa), 
                area.map_type, 
                area.map_perm
            );
            self.push(new_area, None);
        }
    }


}

impl<G: GuestPageTable> GuestMemorySet<G> {
    /// 为 guest page table 新建根页表
    /// 需要分配 16 KiB 对齐的页表
    pub fn new_guest_bare() -> Self {
        Self {
            page_table: GuestPageTable::new_guest(),
            areas: Vec::new()
        }
    }

    pub fn new_guest(
        guest_data: &[u8], 
        gpm_size: usize, 
        guest_machine: &MachineMeta
    ) -> Self {
        let mut gpm = Self::new_guest_bare();
        let elf = xmas_elf::ElfFile::new(guest_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        let mut paddr = GUEST_START_PA as *mut u8;
        let mut last_paddr = GUEST_START_PA as *mut u8;
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize).into();
                let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize).into();
                hdebug!("va: [{:#x}: {:#x})", start_va.0, end_va.0);
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                // 将内存拷贝到对应的物理内存上
                unsafe{
                    core::ptr::copy(guest_data.as_ptr().add(ph.offset() as usize), paddr, ph.file_size() as usize);
                    let page_align_size = ((ph.mem_size() as usize + PAGE_SIZE - 1) >> 12) << 12;
                    paddr = paddr.add(page_align_size);
                }
                
                let map_area = MapArea::new(
                    start_va, 
                    end_va, 
                    Some(PhysAddr(last_paddr as usize)),
                    Some(PhysAddr(paddr as usize)),
                    MapType::Linear, 
                    map_perm
                );
                hdebug!("va: [{:#x}: {:#x}], pa: [{:#x}: {:#x}]", start_va.0, end_va.0, last_paddr as usize, paddr as usize);
                last_paddr = paddr;
                gpm.push(map_area, None);
            }
            
        }
        let offset = paddr as usize - GUEST_START_PA;

        let guest_end_pa = GUEST_START_PA + gpm_size;
        let guest_end_va = GUEST_START_VA + gpm_size; 
        // 映射其他物理内存
        gpm.push(MapArea::new(
                VirtAddr(offset + GUEST_START_VA), 
                VirtAddr(guest_end_va), 
                Some(PhysAddr(paddr as usize)), 
                Some(PhysAddr(guest_end_pa)), 
                MapType::Linear, 
                MapPermission::R | MapPermission::W | MapPermission::U | MapPermission::X
            ),
            None
        );
        hdebug!("guest va -> [{:#x}: {:#x}), guest pa -> [{:#x}: {:#x})", GUEST_START_VA, guest_end_va, GUEST_START_PA, guest_end_pa);

        gpm.map_trampoline();
        
        // map qemu test
        if let Some(test) = &guest_machine.test_finisher_address {
            gpm.push(
                MapArea::new(
                    test.base_address.into(),
                    (test.base_address + test.size).into(),
                    Some(test.base_address.into()),
                    Some((test.base_address + test.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        // map virtio device
        for virtio_dev in guest_machine.virtio.iter() {
            gpm.push(
                MapArea::new(
                    virtio_dev.base_address.into(),
                    (virtio_dev.base_address + virtio_dev.size).into(),
                    Some(virtio_dev.base_address.into()),
                    Some((virtio_dev.base_address + virtio_dev.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ),
                None,
            )
        }


        if let Some(uart) = &guest_machine.uart {
            gpm.push(
                MapArea::new(
                    uart.base_address.into(),
                    (uart.base_address + uart.size).into(),
                    Some(uart.base_address.into()),
                    Some((uart.base_address + uart.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        if let Some(clint) = &guest_machine.clint {
            gpm.push(
                MapArea::new(
                    clint.base_address.into(),
                    (clint.base_address + clint.size).into(),
                    Some(clint.base_address.into()),
                    Some((clint.base_address + clint.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        if let Some(plic) = &guest_machine.plic {
            gpm.push(
                MapArea::new(
                    plic.base_address.into(),
                    (plic.base_address).into(),
                    Some(plic.base_address.into()),
                    Some((plic.base_address).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        gpm
    }

    pub fn new_guest_without_load(guest_machine: &MachineMeta) -> Self {
        let mut gpm = Self::new_guest_bare();

        htracking!("map guest: [{:#x}: {:#x}]", guest_machine.physical_memory_offset, guest_machine.physical_memory_offset + guest_machine.physical_memory_size);
        gpm.push(MapArea::new(
                VirtAddr(guest_machine.physical_memory_offset -0x20_0000), 
                VirtAddr(guest_machine.physical_memory_offset + guest_machine.physical_memory_size), 
                Some(PhysAddr(guest_machine.physical_memory_offset - 0x20_0000)), 
                Some(PhysAddr(guest_machine.physical_memory_offset + guest_machine.physical_memory_size)), 
                MapType::Linear, 
                MapPermission::R | MapPermission::W | MapPermission::U | MapPermission::X
            ),
            None
        );
        hdebug!("guest va -> [{:#x}: {:#x}), guest pa -> [{:#x}: {:#x})", guest_machine.physical_memory_offset, guest_machine.physical_memory_offset + guest_machine.physical_memory_size, guest_machine.physical_memory_offset, guest_machine.physical_memory_offset + guest_machine.physical_memory_size);

        gpm.map_trampoline();
        
        // map qemu test
        if let Some(test) = &guest_machine.test_finisher_address {
            gpm.push(
                MapArea::new(
                    test.base_address.into(),
                    (test.base_address + test.size + 0x1000).into(),
                    Some(test.base_address.into()),
                    Some((test.base_address + test.size + 0x1000).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U | MapPermission::X,
                ), 
                None
            );
        }

        // map virtio device
        for virtio_dev in guest_machine.virtio.iter() {
            gpm.push(
                MapArea::new(
                    virtio_dev.base_address.into(),
                    (virtio_dev.base_address + virtio_dev.size).into(),
                    Some(virtio_dev.base_address.into()),
                    Some((virtio_dev.base_address + virtio_dev.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ),
                None,
            )
        }


        if let Some(uart) = &guest_machine.uart {
            gpm.push(
                MapArea::new(
                    uart.base_address.into(),
                    (uart.base_address + uart.size).into(),
                    Some(uart.base_address.into()),
                    Some((uart.base_address + uart.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        if let Some(clint) = &guest_machine.clint {
            gpm.push(
                MapArea::new(
                    clint.base_address.into(),
                    (clint.base_address + clint.size).into(),
                    Some(clint.base_address.into()),
                    Some((clint.base_address + clint.size).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        if let Some(plic) = &guest_machine.plic {
            gpm.push(
                MapArea::new(
                    plic.base_address.into(),
                    (plic.base_address + 0x0020_0000).into(),
                    Some(plic.base_address.into()),
                    Some((plic.base_address + 0x0020_0000).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        if let Some(pci) = &guest_machine.pci {
            gpm.push(
                MapArea::new(
                    pci.base_address.into(),
                    (pci.base_address + 0x0020_0000).into(),
                    Some(pci.base_address.into()),
                    Some((pci.base_address + 0x0020_0000).into()),
                    MapType::Linear,
                    MapPermission::R | MapPermission::W | MapPermission::U,
                ), 
                None
            );
        }

        gpm
    }
}

/// map area structure, controls a contiguous piece of virtual memory
#[derive(Clone)]
pub struct MapArea<P: PageTable> {
    pub vpn_range: VPNRange,
    pub ppn_range: Option<PPNRange>,
    pub data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    pub map_type: MapType,
    pub map_perm: MapPermission,
    _marker: PhantomData<P>
}

impl<P> MapArea<P> where P: PageTable {
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        start_pa: Option<PhysAddr>,
        end_pa: Option<PhysAddr>,
        map_type: MapType,
        map_perm: MapPermission,
    ) -> Self {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        if let (Some(start_pa), Some(end_pa)) = (start_pa, end_pa) {
            let start_ppn = start_pa.floor();
            let end_ppn = end_pa.ceil();
            return Self {
                vpn_range: VPNRange::new(start_vpn, end_vpn),
                ppn_range: Some(PPNRange::new(start_ppn, end_ppn)),
                data_frames: BTreeMap::new(),
                map_type,
                map_perm,
                _marker: PhantomData
            }
        }
        Self{
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            ppn_range: None,
            data_frames: BTreeMap::new(),
            map_type,
            map_perm,
            _marker: PhantomData
        }
    }
    pub fn map_one(&mut self, page_table: &mut P, vpn: VirtPageNum, ppn_: Option<PhysPageNum>) {
        let ppn: PhysPageNum;
        match self.map_type {
            // 线性映射
            MapType::Linear => {
                ppn = ppn_.unwrap();
            },
            MapType::Framed => {
                let frame = frame_alloc().unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
        }
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        page_table.map(vpn, ppn, pte_flags);
    }
    #[allow(unused)]
    pub fn unmap_one(&mut self, page_table: &mut P, vpn: VirtPageNum) {
        if self.map_type == MapType::Framed {
            self.data_frames.remove(&vpn);
        }
        page_table.unmap(vpn);
    }
    pub fn map(&mut self, page_table: &mut P) {
        let vpn_range = self.vpn_range;
        if let Some(ppn_range) = self.ppn_range {
            let ppn_start: usize = ppn_range.get_start().into();
            let ppn_end: usize = ppn_range.get_end().into();
            let vpn_start: usize = vpn_range.get_start().into();
            let vpn_end: usize = vpn_range.get_end().into();
            assert_eq!(ppn_end - ppn_start, vpn_end - vpn_start);
            let mut ppn = ppn_range.get_start();
            let mut vpn = vpn_range.get_start();
            loop {
                self.map_one(page_table, vpn, Some(ppn));
                ppn.step();
                vpn.step();
                if ppn == ppn_range.get_end() && vpn == vpn_range.get_end() {
                    break;
                }
            }
        }else{
            for vpn in self.vpn_range {
                self.map_one(page_table, vpn, None)
            }
        }
    }
    #[allow(unused)]
    pub fn unmap(&mut self, page_table: &mut P) {
        for vpn in self.vpn_range {
            self.unmap_one(page_table, vpn);
        }
    }
    /// data: start-aligned but maybe with shorter length
    /// assume that all frames were cleared before
    pub fn copy_data(&mut self, page_table: &mut P, data: &[u8]) {
        assert_eq!(self.map_type, MapType::Framed);
        let mut start: usize = 0;
        let mut current_vpn = self.vpn_range.get_start();
        let len = data.len();
        loop {
            let src = &data[start..len.min(start + PAGE_SIZE)];
            let dst = &mut page_table
                .translate(current_vpn)
                .unwrap()
                .ppn()
                .get_bytes_array()[..src.len()];
            dst.copy_from_slice(src);
            start += PAGE_SIZE;
            if start >= len {
                break;
            }
            current_vpn.step();
        }
    }

}

#[derive(Copy, Clone, PartialEq, Debug)]
/// map type for memory set: identical or framed
pub enum MapType {
    Framed,
    Linear
}

bitflags! {
    /// map permission corresponding to that in pte: `R W X U`
    pub struct MapPermission: u8 {
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
    }
}

#[allow(unused)]
pub fn remap_test() {
    let host_vmm = unsafe{ HOST_VMM.get().unwrap().lock() };
    let kernel_space = &host_vmm.hpm;
    let mid_text: VirtAddr = ((stext as usize + etext as usize) / 2).into();
    let mid_rodata: VirtAddr = ((srodata as usize + erodata as usize) / 2).into();
    let mid_data: VirtAddr = ((sdata as usize + edata as usize) / 2).into();

    assert!(!kernel_space
        .page_table
        .translate(mid_text.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_rodata.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_data.floor())
        .unwrap()
        .executable(),);
    unsafe{ core::ptr::read(TRAMPOLINE as *const usize) };
    // 测试 guest ketnel
    hdebug!("remap test passed!");
    drop(host_vmm);
}

// #[allow(unused)]
// pub fn guest_kernel_test() {
//     use crate::constants::layout::GUEST_KERNEL_PHY_START_1;
//     let mut kernel_space = HYPERVISOR_MEMORY.exclusive_access();

//     let guest_kernel_text: VirtAddr = GUEST_KERNEL_PHY_START_1.into();

//     assert!(kernel_space.page_table.translate(guest_kernel_text.floor()).unwrap().executable());
//     assert!(kernel_space.page_table.translate(guest_kernel_text.floor()).unwrap().readable());
//     // 尝试读数据
//     unsafe{
//         core::ptr::read(GUEST_KERNEL_PHY_START_1 as *const u32);
//     }
//     // 测试 guest ketnel
//     hdebug!("guest kernel test passed!");
// }