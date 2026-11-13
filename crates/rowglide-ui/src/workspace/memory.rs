//! Whole-process resident memory sampling; no per-tab or disk accounting.

use super::Workspace;
use gpui_kit::*;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

impl Workspace {
    /// Sample only this process once per second on a background executor.
    /// Reuse the system snapshot and allow at most one sample in flight. The owned
    /// task stops with the workspace; unavailable readings replace stale values.
    /// Initialization also runs in the background; failure leaves the unavailable
    /// label in place and is logged without affecting query execution.
    pub(super) fn watch_memory(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut system = match cx
                .background_executor()
                .spawn(async { System::new() })
                .await
            {
                Ok(system) => system,
                Err(error) => {
                    eprintln!("Cannot initialize memory monitoring: {error}");
                    return;
                }
            };
            loop {
                let (updated, bytes) = cx
                    .background_executor()
                    .spawn(async move {
                        let bytes = sample(&mut system);
                        (system, bytes)
                    })
                    .await;
                system = updated;
                if this
                    .update(cx, |this, cx| {
                        if this.memory_bytes != bytes {
                            this.memory_bytes = bytes;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
    }

    /// Render the last sampled RSS in binary units without performing OS work during drawing.
    /// This is whole-process memory, including all tabs, the engine and the UI.
    pub(super) fn render_memory(&self) -> impl IntoElement {
        let label = match self.memory_bytes {
            Some(bytes) if bytes >= 1024 * 1024 * 1024 => {
                format!("Memory {:.2} GiB", bytes as f64 / (1024. * 1024. * 1024.))
            }
            Some(bytes) => format!("Memory {:.1} MiB", bytes as f64 / (1024. * 1024.)),
            None => "Memory —".into(),
        };
        div()
            .flex_shrink_0()
            .text_xs()
            .text_color(rgb(0x6c7b73))
            .child(label)
    }
}

/// Refresh the current PID's resident memory only; unsupported or failed reads return None.
/// The caller must run this on a background worker, never in a render callback.
fn sample(system: &mut System) -> Option<u64> {
    let pid = Pid::from_u32(std::process::id());
    let refreshed = system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory().without_tasks(),
    );
    if refreshed == 0 {
        return None;
    }
    system
        .process(pid)
        .map(|process| process.memory())
        .filter(|bytes| *bytes > 0)
}

#[cfg(test)]
mod tests {
    use super::sample;
    use sysinfo::{Pid, System};

    /// Repeated sampling reads live memory and keeps only this process in the snapshot.
    #[test]
    fn samples_current_process_only() -> Result<(), sysinfo::Error> {
        let mut system = match System::new() {
            Ok(system) => system,
            Err(sysinfo::Error::Unsupported) => return Ok(()),
            Err(error) => return Err(error),
        };
        for _ in 0..2 {
            let bytes = sample(&mut system);
            assert!(bytes.is_some_and(|bytes| bytes > 0));
            assert_eq!(system.processes().len(), 1);
            assert!(system.process(Pid::from_u32(std::process::id())).is_some());
        }
        Ok(())
    }
}
