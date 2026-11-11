//! Progress and report output.

use std::{fmt, io::IsTerminal, time::Duration};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Pipeline stage of one package.
#[derive(Debug, Clone, Copy)]
pub enum Stage {
    Check,
    Fetch,
    Derive,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Check => "check",
            Self::Fetch => "fetch",
            Self::Derive => "derive",
        })
    }
}

/// Live progress on stderr: one status line per package plus a total bar on a
/// terminal, one line per stage change otherwise.
pub struct Ui {
    live: Option<Live>,
}

struct Live {
    multi: MultiProgress,
    total: ProgressBar,
    width: usize,
}

impl Ui {
    /// # Panics
    ///
    /// Never; the progress templates are constant and valid.
    #[must_use]
    pub fn new(names: &[String]) -> Self {
        let live = std::io::stderr().is_terminal().then(|| {
            let multi = MultiProgress::new();
            let total = multi.add(ProgressBar::new(names.len() as u64));
            total.set_style(
                ProgressStyle::with_template("[{bar:30}] {pos}/{len} {elapsed}")
                    .expect("the total bar template is valid")
                    .progress_chars("=> "),
            );
            Live {
                multi,
                total,
                width: names.iter().map(String::len).max().unwrap_or(0),
            }
        });
        Self { live }
    }

    /// # Panics
    ///
    /// Never; the progress templates are constant and valid.
    #[must_use]
    pub fn package(&self, name: &str) -> Progress {
        let bar = self.live.as_ref().map(|live| {
            let bar = live
                .multi
                .insert_before(&live.total, ProgressBar::new_spinner());
            bar.set_style(
                ProgressStyle::with_template(&format!(
                    "{{spinner}} {{prefix:{}}} {{msg}}",
                    live.width
                ))
                .expect("the package line template is valid"),
            );
            bar.set_prefix(name.to_owned());
            bar.set_message("queued");
            bar.enable_steady_tick(Duration::from_millis(100));
            (bar, live.total.clone())
        });
        Progress {
            name: name.to_owned(),
            bar,
        }
    }

    /// Removes the live display so the final report starts on a clean screen.
    pub fn finish(self) {
        if let Some(live) = self.live {
            live.total.finish();
            // The display is cosmetic; a failed clear must not fail the run.
            let _ = live.multi.clear();
        }
    }
}

/// Progress handle of one package.
pub struct Progress {
    name: String,
    bar: Option<(ProgressBar, ProgressBar)>,
}

impl Progress {
    pub fn stage(&self, stage: Stage) {
        match &self.bar {
            Some((bar, _)) => bar.set_message(stage.to_string()),
            None => eprintln!("{}: {stage}", self.name),
        }
    }

    pub fn done(&self, ok: bool) {
        let status = if ok { "done" } else { "failed" };
        match &self.bar {
            Some((bar, total)) => {
                bar.finish_with_message(status);
                total.inc(1);
            }
            None => eprintln!("{}: {status}", self.name),
        }
    }
}
