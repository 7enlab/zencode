# JobKill

Stop a background Bash job by its job id.

## Parameters

- `job_id` (string, required): Job id returned by `Bash` with
  `background: true`.

## Behavior

- Stops the job and the processes it started.
- Use it for runaway commands; a finished job needs no kill.

## Phase bundle

`exec` (verification). Related: `Bash`, `JobOutput`.
