# TodoWrite

Replace the session task list.

## Parameters

- `todos` (array, required): The full new task list. Each item has `content`
  (string) and `status` (`pending`, `in_progress`, or `completed`).

## Behavior

- Use it to plan multi-step work up front and to track progress.
- Keep exactly one item `in_progress` while you work; mark items `completed`
  as you finish them.
- The list is session-scoped and also drives the agent's own progress nudges.

## Examples

```json
{
  "todos": [
    { "content": "Reproduce the bug", "status": "completed" },
    { "content": "Fix the root cause", "status": "in_progress" },
    { "content": "Add a regression test", "status": "pending" }
  ]
}
```

## Phase bundle

`core` — always sent with a full schema, in every bundle.
