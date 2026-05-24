use raw_cpuid::CpuId;
use sysinfo::System;
use tracing::{info, warn};
use std::collections::BTreeSet;

pub fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "unknown"
    }
}

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CpuInfo {
    pub brand: String,
    pub logical_cores: usize,
    pub physical_cores: usize,
    pub l2_cache: u64,
    pub l3_cache: u64,
    pub numa_nodes: usize,
    pub aes_ni: bool,
    pub avx2: bool,
    pub avx512f: bool,
    pub sha_ext: bool,
    pub total_memory: u64,
    pub free_memory: u64,
}

#[derive(Debug, Clone)]
pub struct ThreadPlan {
    pub p_core_pus: Vec<usize>,
    pub e_core_pus: Vec<usize>,
}

impl ThreadPlan {
    pub fn preferred_p_only(&self) -> Vec<usize> {
        if self.p_core_pus.is_empty() {
            return self.all_pus();
        }
        self.p_core_pus.clone()
    }

    pub fn preferred_with_e(&self) -> Vec<usize> {
        let mut out = self.preferred_p_only();
        for pu in &self.e_core_pus {
            if !out.contains(pu) {
                out.push(*pu);
            }
        }
        out
    }

    pub fn all_pus(&self) -> Vec<usize> {
        let mut s = BTreeSet::new();
        for pu in &self.p_core_pus {
            s.insert(*pu);
        }
        for pu in &self.e_core_pus {
            s.insert(*pu);
        }
        s.into_iter().collect()
    }
}

impl CpuInfo {
    pub fn detect() -> Self {
        let cpuid = CpuId::new();
        let mut sys = System::new();
        sys.refresh_cpu();
        sys.refresh_memory();

        let logical_cores = num_cpus::get();
        let physical_cores = sys.physical_core_count().unwrap_or(logical_cores);

        let brand = cpuid
            .get_processor_brand_string()
            .map(|b| b.as_str().trim().to_string())
            .unwrap_or_else(|| "Unknown CPU".to_string());

        let features = cpuid.get_feature_info();
        let ext_features = cpuid.get_extended_feature_info();

        let aes_ni = features.map(|f| f.has_aesni()).unwrap_or(false);
        let avx2 = ext_features.as_ref().map(|f| f.has_avx2()).unwrap_or(false);
        let avx512f = ext_features
            .as_ref()
            .map(|f| f.has_avx512f())
            .unwrap_or(false);
        let sha_ext = ext_features.as_ref().map(|f| f.has_sha()).unwrap_or(false);

        let (l2_cache, l3_cache, numa_nodes) = detect_cache_topo();

        // sysinfo >= 0.29 returns bytes directly
        let total_memory = sys.total_memory();
        let free_memory = sys.free_memory();

        CpuInfo {
            brand,
            logical_cores,
            physical_cores,
            l2_cache,
            l3_cache,
            numa_nodes,
            aes_ni,
            avx2,
            avx512f,
            sha_ext,
            total_memory,
            free_memory,
        }
    }

    pub fn print_summary(&self) {
        info!(
            "* CPU          {} {}C/{}T L2:{:.1} MB L3:{:.1} MB NUMA:{} {}{} x86-64",
            self.brand,
            self.physical_cores,
            self.logical_cores,
            self.l2_cache as f64 / 1_048_576.0,
            self.l3_cache as f64 / 1_048_576.0,
            self.numa_nodes,
            if self.aes_ni { "AES " } else { "" },
            if self.avx2 { "AVX2" } else { "" },
        );

        info!(
            "* MEMORY       {:.1}/{:.1} GB",
            (self.total_memory - self.free_memory) as f64 / 1_073_741_824.0,
            self.total_memory as f64 / 1_073_741_824.0,
        );

        info!("* DONATE       0%");
    }

    /// Default to physical cores for RandomX. Users can still override with -t/--threads.
    pub fn recommended_threads(&self) -> u32 {
        self.physical_cores.max(1) as u32
    }

    pub fn build_thread_plan(&self) -> ThreadPlan {
        build_thread_plan()
    }
}

fn detect_cache_topo() -> (u64, u64, usize) {
    use hwlocality::object::attributes::ObjectAttributes;
    use hwlocality::object::types::ObjectType;
    let topo = match hwlocality::Topology::new() {
        Ok(t) => t,
        Err(e) => {
            warn!("hwloc init failed: {}, using fallback", e);
            return (0, 0, 1);
        }
    };

    let l2 = topo
        .objects_with_type(ObjectType::L2Cache)
        .filter_map(|obj| match obj.attributes() {
            Some(ObjectAttributes::Cache(attr)) => attr.size().map(|s| s.get()),
            _ => None,
        })
        .min()
        .unwrap_or(0);

    let l3 = topo
        .objects_with_type(ObjectType::L3Cache)
        .next()
        .and_then(|obj| match obj.attributes() {
            Some(ObjectAttributes::Cache(attr)) => attr.size().map(|s| s.get()),
            _ => None,
        })
        .unwrap_or(0);

    let nodes = topo.objects_with_type(ObjectType::NUMANode).count().max(1);

    (l2, l3, nodes)
}

fn build_thread_plan() -> ThreadPlan {
    let topo = match hwlocality::Topology::new() {
        Ok(t) => t,
        Err(e) => {
            warn!("hwloc init failed for thread plan: {}, using fallback", e);
            let fallback = (0..num_cpus::get()).collect::<Vec<_>>();
            return ThreadPlan {
                p_core_pus: fallback,
                e_core_pus: Vec::new(),
            };
        }
    };

    let (mut p, mut e) = split_p_e_by_core_shape(&topo);

    if p.is_empty() && e.is_empty() {
        let all = one_pu_per_core(&topo, &topo.cpuset());
        return ThreadPlan {
            p_core_pus: all,
            e_core_pus: Vec::new(),
        };
    }

    if p.is_empty() {
        p = e.clone();
        e.clear();
    }

    info!(
        "* TOPOLOGY     P-core PUs: {:?} E-core PUs: {:?}",
        p, e
    );
    ThreadPlan {
        p_core_pus: p,
        e_core_pus: e,
    }
}

fn one_pu_per_core(topo: &hwlocality::Topology, cpuset: &hwlocality::cpu::cpuset::CpuSet) -> Vec<usize> {
    use hwlocality::object::types::ObjectType;

    let mut seen_cores = BTreeSet::new();
    let mut out = Vec::new();
    for pu_os in cpuset.iter_set() {
        let pu_os = usize::from(pu_os);
        let Some(pu_obj) = topo.pu_with_os_index(pu_os) else {
            continue;
        };
        let mut core_key = None;
        for a in pu_obj.ancestors() {
            if a.object_type() == ObjectType::Core {
                core_key = a.os_index().or(Some(a.global_persistent_index() as usize));
                break;
            }
        }
        let key = core_key.unwrap_or(pu_os);
        if seen_cores.insert(key) {
            out.push(pu_os);
        }
    }
    out.sort_unstable();
    out
}

fn split_p_e_by_core_shape(topo: &hwlocality::Topology) -> (Vec<usize>, Vec<usize>) {
    use hwlocality::object::types::ObjectType;
    let mut smt2 = Vec::new();
    let mut smt1 = Vec::new();

    for core in topo.objects_with_type(ObjectType::Core) {
        let mut pus = core
            .cpuset()
            .map(|s| s.iter_set().map(usize::from).collect::<Vec<_>>())
            .unwrap_or_default();
        pus.sort_unstable();
        pus.dedup();
        if let Some(&first_pu) = pus.first() {
            if pus.len() >= 2 {
                smt2.push(first_pu);
            } else {
                smt1.push(first_pu);
            }
        }
    }

    smt2.sort_unstable();
    smt1.sort_unstable();

    if !smt2.is_empty() {
        (smt2, smt1)
    } else {
        (smt1, Vec::new())
    }
}
