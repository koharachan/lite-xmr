use rayon::prelude::*;
use std::ffi::c_void;
use std::os::raw::{c_uint, c_ulong};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::debug;

const FLAG_FULL_MEM: u32 = 0x04;
const FLAG_LARGE_PAGES: u32 = 0x01;
pub const HASH_SIZE: usize = 32;
static RANDOMX_HEADER_LOGGED: OnceLock<()> = OnceLock::new();

#[repr(C)]
struct RandomXCache {
    _private: [u8; 0],
}
unsafe impl Send for RandomXCache {}
unsafe impl Sync for RandomXCache {}

#[repr(C)]
struct RandomXDataset {
    _private: [u8; 0],
}

#[repr(C)]
struct RandomXVm {
    _private: [u8; 0],
}

#[link(name = "randomx", kind = "static")]
unsafe extern "C" {
    fn randomx_get_flags() -> c_uint;
    fn randomx_alloc_cache(flags: c_uint) -> *mut RandomXCache;
    fn randomx_init_cache(cache: *mut RandomXCache, key: *const c_void, key_size: usize);
    fn randomx_release_cache(cache: *mut RandomXCache);

    fn randomx_alloc_dataset(flags: c_uint) -> *mut RandomXDataset;
    fn randomx_dataset_item_count() -> c_ulong;
    fn randomx_init_dataset(
        dataset: *mut RandomXDataset,
        cache: *mut RandomXCache,
        start_item: c_ulong,
        item_count: c_ulong,
    );
    fn randomx_release_dataset(dataset: *mut RandomXDataset);

    fn randomx_create_vm(
        flags: c_uint,
        cache: *mut RandomXCache,
        dataset: *mut RandomXDataset,
    ) -> *mut RandomXVm;
    fn randomx_destroy_vm(machine: *mut RandomXVm);

    fn randomx_calculate_hash(
        machine: *mut RandomXVm,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
    );
}

pub fn recommended_flags() -> u32 {
    unsafe { randomx_get_flags() as u32 }
}

fn ensure_header_compat() -> anyhow::Result<()> {
    let c_hash_size = crate::native_bridge::randomx_hash_size();
    if c_hash_size != HASH_SIZE {
        anyhow::bail!(
            "RandomX hash size mismatch: Rust={} C-header={}",
            HASH_SIZE,
            c_hash_size
        );
    }

    RANDOMX_HEADER_LOGGED.get_or_init(|| {
        debug!(
            "RandomX header check ok: hash_size={} dataset_max={}",
            c_hash_size,
            crate::native_bridge::randomx_dataset_max_size()
        );
    });

    Ok(())
}

pub struct Dataset {
    ptr: NonNull<RandomXDataset>,
    algo: String,
}

unsafe impl Send for Dataset {}
unsafe impl Sync for Dataset {}

impl Dataset {
    pub fn new(seed: &[u8]) -> anyhow::Result<Arc<Self>> {
        Self::new_for_algo("rx/0", seed)
    }

    pub fn new_for_algo(algo: &str, seed: &[u8]) -> anyhow::Result<Arc<Self>> {
        ensure_header_compat()?;

        if !crate::native_bridge::randomx_apply_config(algo) {
            anyhow::bail!("unsupported RandomX algorithm '{}'", algo);
        }

        if seed.is_empty() {
            anyhow::bail!("RandomX seed is empty");
        }

        let flags = recommended_flags();
        let cache = NonNull::new(unsafe { randomx_alloc_cache(flags | FLAG_LARGE_PAGES) })
            .or_else(|| NonNull::new(unsafe { randomx_alloc_cache(flags) }))
            .ok_or_else(|| anyhow::anyhow!("failed to allocate RandomX cache"))?;
        unsafe {
            randomx_init_cache(cache.as_ptr(), seed.as_ptr().cast(), seed.len());
        }

        let dataset = NonNull::new(unsafe { randomx_alloc_dataset(FLAG_LARGE_PAGES) })
            .or_else(|| NonNull::new(unsafe { randomx_alloc_dataset(0) }))
            .ok_or_else(|| anyhow::anyhow!("failed to allocate RandomX dataset"))?;

        let total_items = unsafe { randomx_dataset_item_count() };
        let cache_ptr = cache.as_ptr() as usize;
        let dataset_ptr = dataset.as_ptr() as usize;
        let workers = rayon::current_num_threads().clamp(1, 64);
        let chunk = (total_items as usize).div_ceil(workers) as c_ulong;
        (0..workers).into_par_iter().for_each(|i| {
            let start = (i as c_ulong).saturating_mul(chunk);
            if start < total_items {
                let count = (total_items - start).min(chunk);
                unsafe {
                    randomx_init_dataset(
                        dataset_ptr as *mut RandomXDataset,
                        cache_ptr as *mut RandomXCache,
                        start,
                        count,
                    );
                }
            }
        });
        unsafe { randomx_release_cache(cache.as_ptr()) };

        Ok(Arc::new(Dataset {
            ptr: dataset,
            algo: algo.to_string(),
        }))
    }
}

impl Drop for Dataset {
    fn drop(&mut self) {
        unsafe {
            randomx_release_dataset(self.ptr.as_ptr());
        }
    }
}

pub struct Vm {
    ptr: NonNull<RandomXVm>,
    _dataset: Arc<Dataset>,
}

impl Vm {
    pub fn new(dataset: Arc<Dataset>) -> anyhow::Result<Self> {
        if !crate::native_bridge::randomx_apply_config(&dataset.algo) {
            anyhow::bail!("unsupported RandomX algorithm '{}'", dataset.algo);
        }
        let flags = recommended_flags() | FLAG_FULL_MEM;
        let ptr = NonNull::new(unsafe {
            randomx_create_vm(
                flags | FLAG_LARGE_PAGES,
                std::ptr::null_mut(),
                dataset.ptr.as_ptr(),
            )
        })
        .or_else(|| {
            NonNull::new(unsafe {
                randomx_create_vm(flags, std::ptr::null_mut(), dataset.ptr.as_ptr())
            })
        })
        .ok_or_else(|| anyhow::anyhow!("failed to create RandomX VM"))?;
        Ok(Vm {
            ptr,
            _dataset: dataset,
        })
    }

    pub fn hash_batch<const N: usize>(
        &self,
        inputs: [&[u8]; N],
        outputs: &mut [[u8; HASH_SIZE]; N],
    ) {
        for i in 0..N {
            self.hash_one(inputs[i], &mut outputs[i]);
        }
    }

    pub fn hash_one(&self, input: &[u8], output: &mut [u8; HASH_SIZE]) {
        unsafe {
            randomx_calculate_hash(
                self.ptr.as_ptr(),
                input.as_ptr().cast(),
                input.len(),
                output.as_mut_ptr().cast(),
            );
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        unsafe {
            randomx_destroy_vm(self.ptr.as_ptr());
        }
    }
}
