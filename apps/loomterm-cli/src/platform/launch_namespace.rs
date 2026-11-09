//! Product/Root-specific launchd names; pure selection is testable without launchctl.
use std::{io, path::Path, time::Duration};

pub(crate) fn label_prefix(runtime: &Path) -> io::Result<String> {
    Ok(format!(
        "dev.loomterm.server.{}.",
        crate::product::pipe_name(runtime)?
    ))
}

pub(crate) fn stale_job_labels<'a>(list: &'a str, now: Duration, prefix: &str) -> Vec<&'a str> {
    list.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let pid = fields.next()?.trim();
            let label = fields.nth(1)?.trim();
            let (process, started) = label.strip_prefix(prefix)?.split_once('.')?;
            process.parse::<u32>().ok()?;
            let started = started.parse::<u128>().ok()?;
            (pid == "-"
                && now.as_nanos().saturating_sub(started) >= Duration::from_secs(60).as_nanos())
            .then_some(label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn m2_launchd_cleanup_selects_only_old_finished_jobs_in_this_root() {
        let base = std::env::temp_dir();
        let a = label_prefix(&base.join("loomterm-m2-a")).unwrap();
        let b = label_prefix(&base.join("loomterm-m2-b")).unwrap();
        let now = Duration::from_secs(1000);
        let old = Duration::from_secs(900).as_nanos();
        let text = format!("-\t0\t{a}12.{old}\n42\t0\t{a}13.{old}\n-\t0\t{b}12.{old}\n-\t0\tdev.herdr.server.12.{old}\n-\t0\t{a}bad.{old}\n-\t0\t{a}14.{}\n",Duration::from_secs(990).as_nanos());
        assert_eq!(
            stale_job_labels(&text, now, &a),
            vec![format!("{a}12.{old}")]
        );
    }
}
