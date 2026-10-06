//! Background-job presentation helpers.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utils::background::LastRun;

/// What a page shows about the last run: when it finished, whether it
/// succeeded, and one line of text.
pub struct LastRunView {
    pub finished_at: String,
    pub ok: bool,
    pub text: String,
}

pub trait LastRunExt<R> {
    fn view(self, describe: impl FnOnce(R) -> String) -> LastRunView;
}

impl<R> LastRunExt<R> for LastRun<R> {
    /// `describe` turns a successful outcome into its display text. An error
    /// outcome shows the error string.
    fn view(self, describe: impl FnOnce(R) -> String) -> LastRunView {
        let (ok, text) = match self.outcome {
            Ok(value) => (true, describe(value)),
            Err(error) => (false, error),
        };
        LastRunView {
            finished_at: format_timestamp(self.finished_at),
            ok,
            text,
        }
    }
}

/// Format a timestamp as RFC3339, truncated to whole seconds.
pub fn format_timestamp(timestamp: OffsetDateTime) -> String {
    timestamp
        .to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .expect("0 ns is valid")
        .format(&Rfc3339)
        .expect("RFC3339 formatting cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn format_timestamp_truncates_to_whole_seconds() {
        assert_eq!(
            format_timestamp(datetime!(2026-08-25 03:31:58.513 UTC)),
            "2026-08-25T03:31:58Z"
        );
    }
}
