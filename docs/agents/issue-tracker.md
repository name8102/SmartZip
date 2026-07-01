# Issue Tracker: Trellis Tasks

Planning and implementation work for this repo lives under `.trellis/tasks/`.

## Conventions

- One task per directory: `.trellis/tasks/<MM-DD-slug>/`
- Required task metadata lives in `task.json`
- The primary planning artifact is `prd.md`
- Complex tasks may also include `design.md` and `implement.md`
- Context manifests may exist as `implement.jsonl` and `check.jsonl`
- Parent/child relationships are tracked in `task.json` and managed through `task.py`

## When a skill says "publish to the issue tracker"

Create a new Trellis task with:

```bash
python3 ./.trellis/scripts/task.py create "<title>" --slug <slug>
```

If the work belongs under an existing planning umbrella, use `--parent <task-dir>`.

## When a skill says "fetch the relevant ticket"

Read the task directory referenced by the user, usually:

- `.trellis/tasks/<task>/prd.md`
- `.trellis/tasks/<task>/design.md`
- `.trellis/tasks/<task>/implement.md`
- `.trellis/tasks/<task>/task.json`

## Status and Flow

- Task lifecycle is managed through `python3 ./.trellis/scripts/task.py ...`
- Planning stays in the task directory until reviewed
- Implementation begins only after the task is started and moved out of planning
