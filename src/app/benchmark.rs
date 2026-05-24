use tracing::info;

use crate::config::Args;
use crate::cpu::CpuInfo;
use crate::miner;

pub fn run(args: &Args) -> anyhow::Result<()> {
    let seconds = args.bench_seconds.unwrap_or(10).max(1);
    let cpu_info = CpuInfo::detect();
    let plan = cpu_info.build_thread_plan();
    let target_pus = if args.use_e_cores {
        plan.preferred_with_e()
    } else {
        plan.preferred_p_only()
    };

    if let Some(threads) = args.threads {
        let t = threads.max(1) as usize;
        let bind = target_pus.iter().copied().take(t).collect::<Vec<_>>();
        miner::run_benchmark(t as u32, seconds, Some(&bind))?;
        return Ok(());
    }

    let max_threads = target_pus.len().max(1);
    let mut best = 0u64;
    let mut best_t = 1usize;
    for t in 1..=max_threads {
        let bind = target_pus.iter().copied().take(t).collect::<Vec<_>>();
        let hr = miner::run_benchmark(t as u32, seconds, Some(&bind))?;
        if hr > best {
            best = hr;
            best_t = t;
        }
    }
    info!(
        "best benchmark point: threads={} hashrate={}",
        best_t,
        crate::stats::format_hashrate(best)
    );

    Ok(())
}
