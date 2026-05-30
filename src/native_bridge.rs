use std::ffi::CString;

unsafe extern "C" {
    fn lx_randomx_hash_size() -> usize;
    fn lx_randomx_dataset_max_size() -> u64;
    fn lx_randomx_apply_config(algo: *const std::os::raw::c_char) -> i32;
    fn lx_rapidjson_minify(
        input: *const std::os::raw::c_char,
        output: *mut std::os::raw::c_char,
        output_capacity: usize,
        output_len: *mut usize,
    ) -> i32;
}

pub fn randomx_hash_size() -> usize {
    unsafe { lx_randomx_hash_size() }
}

pub fn randomx_dataset_max_size() -> u64 {
    unsafe { lx_randomx_dataset_max_size() }
}

pub fn randomx_apply_config(algo: &str) -> bool {
    let Ok(c_algo) = CString::new(algo) else {
        return false;
    };
    unsafe { lx_randomx_apply_config(c_algo.as_ptr()) == 1 }
}

pub fn rapidjson_minify(input: &str) -> Option<String> {
    let c_input = CString::new(input).ok()?;
    let mut cap = input.len().max(256);

    for _ in 0..5 {
        let mut out = vec![0u8; cap];
        let mut out_len = 0usize;
        let ok = unsafe {
            lx_rapidjson_minify(
                c_input.as_ptr(),
                out.as_mut_ptr().cast(),
                out.len(),
                &mut out_len as *mut usize,
            )
        };

        if ok == 1 {
            out.truncate(out_len);
            return String::from_utf8(out).ok();
        }

        cap = cap.saturating_mul(2);
        if cap == 0 {
            break;
        }
    }

    None
}
