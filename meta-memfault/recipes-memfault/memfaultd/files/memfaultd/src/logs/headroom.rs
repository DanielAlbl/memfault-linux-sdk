//
// Copyright (c) Memfault, Inc.
// See License.txt for details
use crate::{
    logs::log_file::{LogFile, LogFileControl},
    util::disk_size::DiskSize,
};
use eyre::Result;
use serde_json::Value;

#[derive(Debug)]
enum Headroom {
    Ok,
    Shortage {
        num_dropped_logs: usize,
        has_rotated: bool,
    },
}

pub trait HeadroomCheck {
    fn check<L: LogFile>(
        &mut self,
        log_timestamp: Option<&Value>,
        log_file_control: &mut impl LogFileControl<L>,
    ) -> Result<bool>;
}

pub struct HeadroomLimiter {
    state: Headroom,
    /// Minimum amount of free space that must be kept available in the mount point in which
    /// log_tmp_path resides. If there is not sufficient head room, logs will be dropped.
    min_headroom: DiskSize,
    get_available_space: Box<dyn FnMut() -> Result<DiskSize> + Send>,
}

impl HeadroomLimiter {
    pub fn new<S: FnMut() -> Result<DiskSize> + Send + 'static>(
        min_headroom: DiskSize,
        get_available_space: S,
    ) -> Self {
        Self {
            state: Headroom::Ok,
            min_headroom,
            get_available_space: Box::new(get_available_space),
        }
    }
}

impl HeadroomCheck for HeadroomLimiter {
    /// Checks whether there is enough headroom to continue writing logs.
    /// If there is not enough headroom, this will flush the current log file and rotate at most
    /// once when needed, until there is enough headroom again. When there's enough space again, it
    /// will emit a log message mentioning the number of dropped logs.
    /// Returns Ok(true) if there is enough headroom, Ok(false) if there is not enough headroom.
    /// It only returns an error if there is an error writing the "Dropped N logs" message.
    fn check<L: LogFile>(
        &mut self,
        log_timestamp: Option<&Value>,
        log_file_control: &mut impl LogFileControl<L>,
    ) -> Result<bool> {
        let available = (self.get_available_space)()?;
        let has_headroom = available.exceeds(&self.min_headroom);

        self.state = match (has_headroom, &self.state) {
            // Enter insufficient headroom state:
            (false, Headroom::Ok) => {
                // Best-effort warning log & flush. If this fails, just keep going.
                let curent_log = log_file_control.current_log();
                let _ = curent_log.write_log(
                    log_timestamp.cloned(),
                    match (
                        available.bytes >= self.min_headroom.bytes,
                        available.inodes >= self.min_headroom.inodes,
                    ) {
                        (false, false) => "Low on disk space and inodes. Starting to drop logs...",
                        (false, true) => "Low on disk space. Starting to drop logs...",
                        (true, false) => "Low on inodes. Starting to drop logs...",
                        _ => unreachable!(),
                    },
                );
                let _ = curent_log.flush();
                Headroom::Shortage {
                    has_rotated: log_file_control.rotate_if_needed().unwrap_or(false),
                    num_dropped_logs: 1,
                }
            }
            // Already in insufficient headroom state:
            (
                false,
                Headroom::Shortage {
                    has_rotated,
                    num_dropped_logs,
                },
            ) => {
                // Rotate logs once only:
                let num_dropped_logs = *num_dropped_logs + 1;
                let has_rotated =
                    *has_rotated || log_file_control.rotate_if_needed().unwrap_or(false);
                Headroom::Shortage {
                    num_dropped_logs,
                    has_rotated,
                }
            }
            // Exit insufficient headroom state:
            (
                true,
                Headroom::Shortage {
                    num_dropped_logs, ..
                },
            ) => {
                let curent_log = log_file_control.current_log();
                curent_log.write_log(
                    log_timestamp.cloned(),
                    format!(
                        "Recovered from low disk space. Dropped {} logs.",
                        num_dropped_logs
                    ),
                )?;
                Headroom::Ok
            }
            // Already in headroom OK state and staying in this state:
            (true, Headroom::Ok) => Headroom::Ok,
        };
        Ok(has_headroom)
    }
}

#[cfg(test)]
mod tests {
    use crate::util::disk_size::DiskSize;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    use super::*;
    use eyre::eyre;
    use rstest::{fixture, rstest};

    #[rstest]
    fn returns_true_if_headroom_ok_and_stays_ok(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl::default();
        fixture.set_available_space(MIN_HEADROOM);

        // Enough headroom: check() returns true and no calls to log_file_control are made:
        assert!(fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());
        assert_eq!(0, log_file_control.logs_written.len());
        assert_eq!(0, log_file_control.flush_count);
        assert_eq!(0, log_file_control.rotation_count);
    }

    #[rstest]
    fn log_upon_enter_and_exit_headroom_space_shortage(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl::default();

        // Enter headroom shortage: check() returns false:
        fixture.set_available_space(MIN_HEADROOM - 1);
        assert!(!fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());

        // Check that the warning log was written:
        assert_eq!(1, log_file_control.logs_written.len());
        assert!(log_file_control.logs_written[0]
            .contains("Low on disk space. Starting to drop logs..."));
        // Check that the log was flushed:
        assert_eq!(1, log_file_control.flush_count);

        // Still not enough headroom: check() returns false:
        assert!(!fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());

        // Recover from headroom shortage: check() returns true again:
        fixture.set_available_space(MIN_HEADROOM);
        assert!(fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());

        // Check that the "recovered" log was written:
        assert_eq!(2, log_file_control.logs_written.len());
        assert!(log_file_control.logs_written[1]
            .contains("Recovered from low disk space. Dropped 2 logs."));
    }

    #[rstest]
    fn log_upon_enter_and_exit_headroom_node_shortage(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl::default();

        // Enter headroom shortage: check() returns false:
        fixture.set_available_inodes(MIN_INODES - 1);
        assert!(!fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());

        // Check that the warning log was written:
        assert_eq!(1, log_file_control.logs_written.len());
        assert!(
            log_file_control.logs_written[0].contains("Low on inodes. Starting to drop logs...")
        );
        // Check that the log was flushed:
        assert_eq!(1, log_file_control.flush_count);

        // Still not enough headroom: check() returns false:
        assert!(!fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());

        // Recover from headroom shortage: check() returns true again:
        fixture.set_available_inodes(MIN_INODES);
        assert!(fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap());

        // Check that the "recovered" log was written:
        assert_eq!(2, log_file_control.logs_written.len());
        assert!(log_file_control.logs_written[1]
            .contains("Recovered from low disk space. Dropped 2 logs."));
    }

    #[rstest]
    fn rotate_once_only_entering_headroom_shortage(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl {
            // Make log_file_control.rotate_if_needed() return Ok(true):
            rotate_return: Some(true),
            ..Default::default()
        };

        // Enter headroom shortage:
        fixture.set_available_space(MIN_HEADROOM - 1);
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 1);

        // Check again. Rotation should not be attempted again:
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 1);
    }

    #[rstest]
    fn rotate_once_only_during_headroom_shortage(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl::default();

        // Enter headroom shortage:
        fixture.set_available_space(MIN_HEADROOM - 1);
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 0);

        // Make log_file_control.rotate_if_needed() return Ok(true):
        log_file_control.rotate_return = Some(true);

        // Check again. Rotation should be attempted again:
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 1);

        // Check again. Rotation should not be attempted again:
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 1);
    }

    #[rstest]
    fn retry_rotate_after_failure(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl {
            // Make log_file_control.rotate_if_needed() return Err(...):
            rotate_return: None,
            ..Default::default()
        };

        // Enter headroom shortage:
        fixture.set_available_space(MIN_HEADROOM - 1);
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 0);

        // Check again. Rotation should be attempted again:
        // Make log_file_control.rotate_if_needed() return Ok(true):
        log_file_control.rotate_return = Some(true);
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        assert_eq!(log_file_control.rotation_count, 1);
    }

    #[rstest]
    fn write_error_of_initial_warning_message_is_ignored(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl::default();

        fixture.set_available_space(MIN_HEADROOM - 1);
        log_file_control.write_should_fail = true;
        assert!(fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .is_ok());
    }

    #[rstest]
    fn write_error_of_recovery_log_message_is_bubbled_up(mut fixture: Fixture) {
        let log_timestamp = Value::from(12345);
        let mut log_file_control = FakeLogFileControl::default();

        fixture.set_available_space(MIN_HEADROOM - 1);
        fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .unwrap();
        fixture.set_available_space(MIN_HEADROOM);
        log_file_control.write_should_fail = true;
        assert!(fixture
            .limiter
            .check(Some(&log_timestamp), &mut log_file_control)
            .is_err());
    }

    struct FakeLogFileControl {
        logs_written: Vec<String>,
        write_should_fail: bool,
        flush_count: usize,
        flush_should_fail: bool,
        /// This controls the result of rotate_if_needed().
        /// Some(...) is mapped to Ok(...) and None is mapped to Err(...).
        rotate_return: Option<bool>,
        /// Number of times actually rotated (rotate_if_needed() calls while rotate_return was Some(true)):
        rotation_count: usize,
    }

    impl Default for FakeLogFileControl {
        fn default() -> Self {
            FakeLogFileControl {
                logs_written: Vec::new(),
                flush_count: 0,
                flush_should_fail: false,
                write_should_fail: false,
                rotate_return: Some(false),
                rotation_count: 0,
            }
        }
    }

    impl LogFile for FakeLogFileControl {
        fn write_json_line(&mut self, json: Value) -> Result<()> {
            if self.write_should_fail {
                Err(eyre!("Write failed"))
            } else {
                self.logs_written.push(serde_json::to_string(&json)?);
                Ok(())
            }
        }

        fn flush(&mut self) -> Result<()> {
            self.flush_count += 1;
            if self.flush_should_fail {
                Err(eyre!("Flush failed"))
            } else {
                Ok(())
            }
        }
    }

    impl LogFileControl<FakeLogFileControl> for FakeLogFileControl {
        fn rotate_if_needed(&mut self) -> Result<bool> {
            match self.rotate_return {
                Some(rv) => {
                    if rv {
                        self.rotation_count += 1;
                    }
                    Ok(rv)
                }
                None => Err(eyre!("Rotate failed")),
            }
        }

        fn rotate_unless_empty(&mut self) -> Result<bool> {
            unimplemented!();
        }

        fn current_log(&mut self) -> &mut FakeLogFileControl {
            self
        }
    }

    struct Fixture {
        available_space: Arc<AtomicU64>,
        available_inodes: Arc<AtomicU64>,
        limiter: HeadroomLimiter,
    }

    impl Fixture {
        fn set_available_space(&mut self, available_space: u64) {
            self.available_space
                .store(available_space, Ordering::Relaxed)
        }
        fn set_available_inodes(&mut self, available_inodes: u64) {
            self.available_inodes
                .store(available_inodes, Ordering::Relaxed)
        }
    }

    const MIN_HEADROOM: u64 = 1024;
    const MIN_INODES: u64 = 10;
    const INITIAL_AVAILABLE_SPACE: u64 = 1024 * 1024;
    const INITIAL_AVAILABLE_INODES: u64 = 100;

    #[fixture]
    fn fixture() -> Fixture {
        let available_space = Arc::new(AtomicU64::new(INITIAL_AVAILABLE_SPACE));
        let available_inodes = Arc::new(AtomicU64::new(INITIAL_AVAILABLE_INODES));

        let space = available_space.clone();
        let inodes = available_inodes.clone();

        Fixture {
            limiter: HeadroomLimiter::new(
                DiskSize {
                    bytes: MIN_HEADROOM,
                    inodes: MIN_INODES,
                },
                move || {
                    Ok(DiskSize {
                        bytes: space.load(Ordering::Relaxed),
                        inodes: inodes.load(Ordering::Relaxed),
                    })
                },
            ),
            available_inodes,
            available_space,
        }
    }
}
