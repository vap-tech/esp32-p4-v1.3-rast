//! Read-only ROM Flash access, no MMU mapping or cached alias. Caller owns Flash
//! operations; no parallel erase/write or other core access. Runtime in SRAM.
#[derive(Debug)]
pub struct Error(pub u32);
/// Read aligned words using the same ROM API as the SPM bootstrap.
/// # Safety
/// Exclusive Flash operation; destination must be internal SRAM. D-cache must
/// be bypassed (ROM writes are not a cache ownership handoff).
pub unsafe fn read(address: u32, out: &mut [u32]) -> Result<(), Error> {
    if !(address as usize).is_multiple_of(4)
        || address as usize + out.len() * 4 > 16 * 1024 * 1024
        || super::memory::cache_snapshot().l1_bypass & 16 == 0
    {
        return Err(Error(u32::MAX));
    }
    let function: unsafe extern "C" fn(u32, *mut u32, i32) -> u32 =
        unsafe { core::mem::transmute(0x4fc0_0158usize) };
    let result = unsafe { function(address, out.as_mut_ptr(), (out.len() * 4) as i32) };
    if result == 0 {
        Ok(())
    } else {
        Err(Error(result))
    }
}
