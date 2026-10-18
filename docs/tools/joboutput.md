# JobOutput

Read the output of a background Bash job.

## Parameters

- `job_id` (string, required): Job id returned by `Bash` with
  `background: true`.
- `wait_ms` (integer, optional): Wait up to this long for the job to finish
  before returning.

## Behavior

- Returns the output printed since your last `JobOutput` call for that job,
  plus whether the job is still running or its exit code.
- Poll it to follow long-running commands (builds, test suites) without
  blocking the session.

## Phase bundle

`exec` (verification). Related: `Bash`, `JobKill`.
