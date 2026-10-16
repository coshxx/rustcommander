//! Running file operations (`crate::ops`) from the UI: start a job, follow its
//! progress, answer conflicts and report the result.

use std::path::PathBuf;
use std::time::Instant;

use crate::app::{App, Mode, TIMER_JOB, Tone, WM_APP_JOB};
use crate::files::names::to_utf16_nul;
use crate::files::temp;
use crate::i18n::{t, tf, tn};
use crate::ops::{self, Answer, Job};
use crate::sys::win::{self, SendHwnd};

/// Short jobs (rename, mkdir) finish before their progress card would appear.
pub(crate) const JOB_CARD_DELAY_MS: u32 = 200;

/// What happens when a job finished successfully.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterJob {
    /// Show the result as a notice.
    Report,
    /// Open the extracted file with its default program (Enter in an archive).
    Open,
    /// Show the extracted file in a lister (F3 in an archive).
    View,
}

pub(crate) struct RunningJob {
    pub(crate) handle: ops::Handle,
    pub(crate) title: &'static str,
    pub(crate) progress: ops::Progress,
    /// Its marks are cleared after a successful job.
    source_panel: Option<usize>,
    pub(crate) started: Instant,
    then: AfterJob,
}

impl App {
    /// Start a file operation; the progress card appears after `JOB_CARD_DELAY_MS`.
    pub(crate) fn start_job(&mut self, job: Job, title: &'static str, source_panel: Option<usize>) {
        let handle = ops::start(job, Some((SendHwnd::new(self.hwnd), WM_APP_JOB)));
        self.job = Some(RunningJob {
            handle,
            title,
            progress: ops::Progress::default(),
            source_panel,
            started: Instant::now(),
            then: AfterJob::Report,
        });
        self.notice = None;
        win::set_timer(self.hwnd, TIMER_JOB, JOB_CARD_DELAY_MS + 10);
    }

    /// Enter or F3 on a file inside an archive: extract it to a temp directory, then open it with
    /// its default program or in a lister.
    pub(crate) fn extract_then(&mut self, panel: usize, path: PathBuf, then: AfterJob) {
        if self.job.is_some() {
            self.notify(t("notice.job_running").into(), Tone::Error);
            return;
        }
        match temp::new_dir() {
            Ok(dir) => {
                let title = t(if then == AfterJob::View { "job.view" } else { "job.open" });
                self.start_job(Job::Extract { sources: vec![path], dest: dir }, title, Some(panel));
                if let Some(job) = &mut self.job {
                    job.then = then;
                }
            }
            Err(e) => self.notify(tf("notice.temp", &[("error", &e)]), Tone::Error),
        }
    }

    /// `WM_APP_JOB`: process everything the worker sent.
    pub fn on_job_event(&mut self) {
        while let Some(job) = &mut self.job
            && let Ok(ev) = job.handle.events.try_recv()
        {
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

    /// Show the result, clear marks on success and refresh both panels.
    fn finish_job(&mut self, job: RunningJob, s: ops::Summary) {
        if matches!(self.mode, Mode::Conflict(_)) {
            self.mode = Mode::Normal;
        }
        if job.then != AfterJob::Report
            && s.errors.is_empty()
            && !s.cancelled
            && let Some(file) = s.select
        {
            match job.then {
                AfterJob::View => self.request_view(file),
                _ => win::shell_open(file),
            }
            return;
        }
        let (text, tone) = if let Some(first) = s.errors.first() {
            let more = if s.errors.len() > 1 { tf("job.more", &[("n", &(s.errors.len() - 1))]) } else { String::new() };
            (format!("{}: {first}{more}", job.title), Tone::Error)
        } else if s.cancelled {
            (tf("job.cancelled", &[("title", &job.title)]), Tone::Info)
        } else {
            let items = tn("dialog.items", s.done);
            (tf("job.done", &[("title", &job.title), ("items", &items)]), Tone::Success)
        };
        self.notify(text, tone);
        if s.errors.is_empty()
            && !s.cancelled
            && let Some(p) = job.source_panel
        {
            self.panels[p].clear_marks();
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

    /// Pass the user's choice in the conflict question to the worker.
    pub(crate) fn answer_conflict(&mut self, a: Answer) {
        if let Some(job) = &self.job {
            job.handle.answer(a);
        }
        self.mode = Mode::Normal;
    }
}
