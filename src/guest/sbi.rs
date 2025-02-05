use super::vmexit::TrapContext;
use crate::VmmResult;
use crate::constants::riscv_regs::GprIndex;
use crate::sbi::leagcy::SBI_SET_TIMER;
use crate::sbi::{
    SBI_EXTID_BASE, SBI_GET_SBI_SPEC_VERSION_FID, SBI_SUCCESS, 
    SBI_PROBE_EXTENSION_FID, SBI_EXTID_TIME, SBI_SET_TIMER_FID, 
    SBI_ERR_NOT_SUPPORTED, console_putchar, console_getchar, set_timer, SBI_CONSOLE_PUTCHAR, SBI_CONSOLE_GETCHAR, 
    SBI_GET_SBI_IMPL_ID_FID, SBI_GET_SBI_IMPL_VERSION_FID, SBI_GET_MVENDORID_FID, SBI_GET_MARCHID_FID, SBI_GET_MIMPID_FID,
};
use sbi_rt;

use riscv::register::{ hvip, sie };
pub struct SbiRet {
    error: usize,
    value: usize
}

#[inline(always)]
pub(crate) fn sbi_call_1(eid: usize, fid: usize, arg0: usize) -> SbiRet {
    let (error, value);
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") eid,
            in("a6") fid,
            inlateout("a0") arg0 => error,
            lateout("a1") value,
        );
    }
    SbiRet { error, value }
}

pub fn sbi_vs_handler(ctx: &mut TrapContext) -> VmmResult {
    let ext_id: usize = ctx.x[GprIndex::A7 as usize];
    let fid: usize = ctx.x[GprIndex::A6 as usize];
    let sbi_ret;

    match ext_id {
        SBI_EXTID_BASE => sbi_ret = sbi_base_handler(fid, ctx),
        SBI_EXTID_TIME => sbi_ret = sbi_time_handler(ctx.x[GprIndex::A0 as usize], fid),
        SBI_CONSOLE_PUTCHAR => sbi_ret = sbi_console_putchar_handler(ctx.x[GprIndex::A0 as usize]),
        SBI_CONSOLE_GETCHAR => sbi_ret = sbi_console_getchar_handler(),
        SBI_SET_TIMER => sbi_ret = sbi_legacy_set_time(ctx.x[GprIndex::A0 as usize]),
        _ => panic!("Unsupported SBI call id {:#x}", ext_id)
    }
    ctx.x[GprIndex::A0 as usize] = sbi_ret.error;
    ctx.x[GprIndex::A1 as usize] = sbi_ret.value;

    Ok(())
    
}

pub fn sbi_base_handler(fid: usize, ctx: &TrapContext) -> SbiRet {
    let mut sbi_ret = SbiRet{
        error: SBI_SUCCESS,
        value: 0
    };
    match fid {
        SBI_GET_SBI_SPEC_VERSION_FID => sbi_ret = sbi_call_1(SBI_EXTID_BASE, fid, 0),
        SBI_GET_SBI_IMPL_ID_FID => sbi_ret.value = sbi_rt::get_sbi_impl_id(),
        SBI_GET_SBI_IMPL_VERSION_FID => sbi_ret.value = sbi_rt::get_sbi_impl_version(),
        SBI_PROBE_EXTENSION_FID => {
            let extension = ctx.x[GprIndex::A0 as usize];
            sbi_ret = sbi_call_1(SBI_EXTID_BASE, fid, extension);
        },
        SBI_GET_MVENDORID_FID => sbi_ret.value = sbi_rt::get_mvendorid(),
        SBI_GET_MARCHID_FID => sbi_ret.value = sbi_rt::get_marchid(),
        SBI_GET_MIMPID_FID => sbi_ret.value = sbi_rt::get_mimpid(),
        _ => panic!("sbi base handler fid: {}", fid)
    }
    sbi_ret
}

pub fn sbi_console_putchar_handler(c: usize) -> SbiRet {
    console_putchar(c);
    return SbiRet { error: SBI_SUCCESS, value: 0 };
}

pub fn sbi_console_getchar_handler() -> SbiRet {
    let c = console_getchar();
    return SbiRet { error: SBI_SUCCESS, value: c };
}

pub fn sbi_time_handler(stime: usize, fid: usize) -> SbiRet {
    let mut sbi_ret = SbiRet {
        error: SBI_SUCCESS,
        value: 0
    };
    if fid != SBI_SET_TIMER_FID {
        sbi_ret.error = SBI_ERR_NOT_SUPPORTED as usize;
        return sbi_ret
    }

    set_timer(stime);
    unsafe{ 
        // clear guest timer interrupt pending
        hvip::clear_vstip(); 
        // enable timer interrupt
        sie::set_stimer();
    }
    return sbi_ret
}

// pub fn sbi_rfence_handler(fid: usize) {

// }

pub fn sbi_legacy_set_time(stime: usize) -> SbiRet {
    let sbi_ret = SbiRet {
        error: SBI_SUCCESS,
        value: 0
    };
    set_timer(stime);
    unsafe{ 
        // clear guest timer interrupt pending
        hvip::clear_vstip(); 
        // enable timer interrupt
        sie::set_stimer();
    }
    return sbi_ret
}