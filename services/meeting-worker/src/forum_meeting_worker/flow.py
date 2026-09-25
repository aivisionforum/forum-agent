"""Cooperative compute flow control; pausing never extends the attempt deadline."""
import threading
import time

from .job_io import JobError


class ComputeFlow:
    def __init__(self, deadline, notify):
        self.deadline, self.notify = deadline, notify
        self.condition = threading.Condition()
        self.paused = False
        self.cancelled = False

    def receive(self, data):
        with self.condition:
            for command in data or b'!':
                if command == ord('P'):
                    self.paused = True
                elif command == ord('R'):
                    self.paused = False
                else:
                    self.cancelled = True
            self.condition.notify_all()

    def check(self):
        announced = False
        with self.condition:
            while True:
                if self.cancelled:
                    raise JobError('CANCELLED', 'Analysis was cancelled.')
                remaining = self.deadline - time.monotonic()
                if remaining <= 0:
                    raise JobError('DEADLINE_EXCEEDED', 'Analysis total budget expired.')
                if not self.paused:
                    if announced:
                        self.notify(False)
                    return
                if not announced:
                    self.notify(True)
                    announced = True
                self.condition.wait(min(remaining, 0.1))
