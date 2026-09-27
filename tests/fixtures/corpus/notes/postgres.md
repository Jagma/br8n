---
title: Postgres migration
tags: [db, migration]
---

# Postgres migration

Moving off MySQL.

## Rejected approaches

This approach failed for three reasons: lock contention,
replication lag, and the connection pooler.

See [[connection-pooling]] for detail.
