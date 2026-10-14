//! Running file operations (`crate::ops`) from the UI: start a job, follow its
//! progress, answer conflicts and report the result.

use std::time::Instant;

use crate::app::{App, Mode, TIMER_JOB, Tone, WM_APP_JOB};
use crate::files::names::to_utf16_nul;
use crate::ops::{self, Answer, Job};
use crate::sys::win::{self, SendHwnd};

/// Short jobs (rename, mkdir) finish before their progress card would appear.
pub(crate) const JOB_CARD_DELAY_MS: u32 = 200;

pub(crate) struct RunningJob {
    pub(crate) handle: ops::Handle,
    pub(crate) title: &'static str,
    pub(crate) progress: ops::Progress,
    source_panel: usize,
    pub(crate) started: Instant,
}

impl App {
    pub(crate) fn start_job(&mut self, job: Job, title: &'static str, source_panel: usize) {
        let handle = ops::start(job, Some((SendHwnd::new(self.hwnd), WM_APP_JOB)));
        self.job = Some(RunningJob {
            handle,
            title,
            progress: ops::Progress::default(),
            source_panel,
            started: Instant::now(),
        });
        self.notice = None;
        win::set_timer(self.hwnd, TIMER_JOB, JOB_CARD_DELAY_MS + 10);
    }

    pub fn on_job_event(&mut self) {
        loop {
            let Some(job) = &mut self.job else { break };
            let Ok(ev) = job.handle.events.try_recv() else { break };
            match ev {
                ops::Event::Progress(p) => job.progress = p,
                ops::Event::Conflict(path) => self.mode = Mode::Conflict(path),
                ops::Event::Done(summary) => {
                    if let Some(job) = self.job.take() {
                        self.finish_job(job, summary);
                    }
                }
            }
        }
        self.invalidate();
    }

    fn finish_job(&mut self, job: RunningJob, s: ops::Summary) {
        if matches!(self.mode, Mode::Conflict(_)) {
            self.mode = Mode::Normal;
        }
        let (text, tone) = if let Some(first) = s.errors.first() {
            let more = if s.errors.len() > 1 { format!(" (+{} weitere)", s.errors.len() - 1) } else { String::new() };
            (format!("{}: {first}{more}", job.title), Tone::Error)
        } else if s.cancelled {
            (format!("{} abgebrochen", job.title), Tone::Info)
        } else {
            let what = if s.done == 1 { "Element" } else { "Elemente" };
            (format!("{}: fertig ({} {what})", job.title, s.done), Tone::Success)
        };
        self.notify(text, tone);
        if s.errors.is_empty() && !s.cancelled {
            self.panels[job.source_panel].clear_marks();
        }
        for i in 0..2 {
            let dir = self.panels[i].dir.clone();
            // Put the cursor on a new/renamed entry if it lives in this panel.
            let select = s
                .select
                .as_ref()
                .and_then(|p| p.strip_prefix(&dir).ok())
                .and_then(|rel| rel.components().next())
                .map(|c| to_utf16_nul(c.as_os_str()))
                .map(|w| w[..w.len() - 1].into());
            match select {
                Some(name) => self.navigate(i, dir, Some(name)),
                None => self.panels[i].reload(i, &self.sink),
            }
        }
    }

    pub(crate) fn answer_conflict(&mut self, a: Answer) {
        if let Some(job) = &self.job {
            job.handle.answer(a);
        }
        self.mode = Mode::Normal;
    }
}
