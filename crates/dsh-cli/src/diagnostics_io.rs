//! Startup diagnostics: summary on the terminal, private report on disk.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use dsh_diagnostics::{
    DiagnosticContext, LOGS_DIR, StartupFailure, log_file_name, log_path, report, saved_path_line,
    summary, write_failure_warning,
};

/// Format a system time as an ISO-8601 UTC timestamp with milliseconds.
#[must_use]
pub fn iso_timestamp(time: SystemTime) -> String {
    let total_millis: i128 = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_millis()).unwrap_or(i128::MAX),
        Err(error) => -i128::try_from(error.duration().as_millis()).unwrap_or(i128::MAX),
    };
    let days = i64::try_from(total_millis.div_euclid(86_400_000)).unwrap_or(0);
    let millis_of_day = total_millis.rem_euclid(86_400_000);
    let millis = millis_of_day % 1_000;
    let seconds_of_day = millis_of_day / 1_000;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Convert days since the Unix epoch to a proleptic Gregorian date.
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Save a startup failure and print the summary and report path.
///
/// A failed save prints the complete report instead of claiming a saved path.
#[must_use]
pub fn report_startup_failure(failure: &StartupFailure, context: &DiagnosticContext) -> String {
    let timestamp = iso_timestamp(SystemTime::now());
    let rendered = report(
        failure,
        context,
        &timestamp,
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let file_name = log_file_name(&timestamp, &uuid::Uuid::new_v4().to_string());
    let directory = Path::new(&context.home).join(LOGS_DIR);
    let path = log_path(&context.home, &file_name);
    let mut output = summary(failure);
    if let Err(error) = write_report(&directory, &path, &rendered) {
        output.push_str(&write_failure_warning(&error));
        output.push_str(&rendered);
        return output;
    }
    output.push_str(&saved_path_line(&path));
    output
}

fn write_report(directory: &Path, path: &str, rendered: &str) -> Result<(), String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot create {path}: {error}"))?;
    file.write_all(rendered.as_bytes())
        .map_err(|error| format!("cannot write {path}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, iso_timestamp, report_startup_failure};
    use dsh_diagnostics::{DiagnosticContext, StartupFailure};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn timestamps_render_utc_with_milliseconds() {
        assert_eq!(iso_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso_timestamp(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123)),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(
            iso_timestamp(UNIX_EPOCH - Duration::from_secs(86_400)),
            "1969-12-31T00:00:00.000Z"
        );
        assert_eq!(
            iso_timestamp(UNIX_EPOCH - Duration::from_secs(2_208_988_800)),
            "1900-01-01T00:00:00.000Z"
        );
        assert_eq!(
            iso_timestamp(UNIX_EPOCH + Duration::from_secs(951_868_800)),
            "2000-03-01T00:00:00.000Z"
        );
        assert_eq!(
            iso_timestamp(UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
            "2100-01-01T00:00:00.000Z"
        );
        assert_eq!(
            iso_timestamp(UNIX_EPOCH + Duration::from_secs(1_706_745_600)),
            "2024-02-01T00:00:00.000Z"
        );
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(-25_567), (1900, 1, 1));
        assert_eq!(civil_from_days(-719_469), (0, 2, 29));
        assert_eq!(civil_from_days(-800_000), (-221, 9, 4));
        assert_eq!(civil_from_days(-1_000_000), (-768, 2, 4));
        assert_eq!(civil_from_days(1_000_000), (4707, 11, 29));
        assert_eq!(civil_from_days(1_500_000), (6076, 11, 10));
        assert_eq!(civil_from_days(2_000_000), (7445, 10, 25));
        assert_eq!(civil_from_days(2_500_000), (8814, 10, 8));
        assert_eq!(civil_from_days(2_932_896), (9999, 12, 31));
        assert_eq!(civil_from_days(2_900_084), (9910, 3, 1));
    }

    #[test]
    fn a_saved_report_is_named_on_stderr_and_written_privately() {
        let home = tempfile::tempdir().expect("tempdir");
        let context = DiagnosticContext {
            home: home.path().display().to_string(),
            version: "1.2.3".to_string(),
            profile: "web".to_string(),
        };
        let failure = StartupFailure {
            message: "dsh: startup failed".to_string(),
            configuration_path: None,
            plugins: Vec::new(),
            messages: Vec::new(),
        };
        let output = report_startup_failure(&failure, &context);
        assert!(output.starts_with("dsh: startup failed\n"));
        assert!(output.contains("Full diagnostics: "));
        let logs = home.path().join("logs");
        let entries: Vec<_> = std::fs::read_dir(&logs).expect("logs").collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("report");
        assert!(text.contains("WARNING: Raw diagnostics"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_failed_save_prints_the_complete_report() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(home.path().join("logs"), "blocked").expect("block");
        let context = DiagnosticContext {
            home: home.path().display().to_string(),
            version: "1.2.3".to_string(),
            profile: "web".to_string(),
        };
        let failure = StartupFailure {
            message: "dsh: startup failed".to_string(),
            configuration_path: None,
            plugins: Vec::new(),
            messages: Vec::new(),
        };
        let output = report_startup_failure(&failure, &context);
        assert!(output.contains("could not write startup diagnostics"));
        assert!(output.contains("Full diagnostics:\n"));
        assert!(output.contains("WARNING: Raw diagnostics"));
    }
}
