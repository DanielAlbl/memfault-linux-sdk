#
# Copyright (c) Memfault, Inc.
# See License.txt for details
import time

from memfault_service_tester import MemfaultServiceTester
from qemu import QEMU


# Assumptions:
# - The machine/qemu is built with a valid project key of a project on app.memfault.com,
#   or whatever the underlying QEMU instance points at.
# - The MEMFAULT_E2E_* environment variables are set to match whatever the underlying
#   QEMU instance points at.
def test(
    qemu: QEMU, memfault_service_tester: MemfaultServiceTester, qemu_device_id: str
):
    qemu.exec_cmd("memfaultctl enable-data-collection")

    # Start following memfaultd logs
    qemu.exec_cmd("journalctl -u memfaultd -n 0 -f &")

    qemu.systemd_wait_for_service_state("memfaultd.service", "active")
    qemu.systemd_wait_for_service_state("collectd.service", "active")

    # Wait until memfaultd has started
    qemu.child().expect("Started memfaultd daemon.")

    # Wait a little bit for any "startup requests"
    time.sleep(3)

    qemu.exec_cmd("memfaultctl request-metrics")

    # Wait for message indicating that collectd has been poked
    qemu.child().expect("collectd:: Requesting metrics from collectd now.")

    # memfaultd waits one second + we want to make sure data has been flushed to memfault and processed
    time.sleep(5)

    # Wait until we have received at least one valid report.
    def _check():
        reports = memfault_service_tester.list_reports(
            dict(device_serial=qemu_device_id),
            ignore_errors=True,
        )
        assert reports
        # Note: sometimes the first heartbeat is an empty dict:
        assert any((report["metrics"] for report in reports))

    memfault_service_tester.poll_until_not_raising(
        _check, timeout_seconds=60, poll_interval_seconds=1
    )

    # Now check how many reports we got
    reports = memfault_service_tester.list_reports(
        dict(device_serial=qemu_device_id),
        ignore_errors=True,
    )
    # And make sure we have at least one
    assert [list(report for report in reports if report["metrics"])]