# Persistent records

This directory stores ordinary task, design, evidence and historical documents.
It has no workflow engine, hooks, mandatory phases or automatic commits.

- `tasks/`: persistent requirements, decisions, progress and verification evidence.
- `workspace/` and existing `spec/`: retained documents; legacy workflow instructions are historical, not current requirements.

Search only records relevant to the request. Update an existing task when useful;
create one for requested tracking or work that needs continuity. Keep the goal,
status, decisions, evidence and remaining work together. Existing JSON metadata
may be edited directly; no task CLI or approval lifecycle is required.
