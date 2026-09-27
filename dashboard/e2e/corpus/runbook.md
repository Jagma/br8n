# Ingest service runbook

The ingest service reads uploads from the inbox bucket, validates each file,
and writes accepted records to the warehouse queue. It runs as three replicas
behind the internal load balancer and pages the on-call engineer when the
queue depth stays above ten thousand for more than five minutes.

## Troubleshooting

When the queue backs up, look at the worker dashboard first. A flat line of
processed records with a rising queue depth almost always means the workers
lost their database connection pool: restart one replica at a time and watch
the processed count recover before restarting the next. Never restart all
three together, because the load balancer drains in-flight uploads and the
clients retry in a burst that makes the backlog worse.

If the workers are processing but slowly, check the validation latency panel.
A single malformed upload with a very large attachment can hold a worker for
minutes; the quarantine command moves it aside so the worker can continue.
Disk pressure on the scratch volume shows up as write errors in the worker log
and clears once the nightly cleanup job has run, which can be triggered by
hand from the scheduler page.

Certificate expiry shows up as handshake failures against the warehouse. The
certificate is renewed automatically fourteen days before it expires, so a
failure here means the renewal job itself failed and needs a manual run.

## Rollback

Every release of the ingest service is a container tag, and the previous tag
stays pinned in the deployment history for thirty days. To roll back, open the
deployment history, pick the last tag that passed its canary, and promote it.
The promotion replaces replicas one at a time and waits for each new replica to
report healthy before moving on, so a rollback takes about four minutes.

Schema migrations are the exception. A release that ran a forward migration
cannot be rolled back by promoting the old tag alone, because the old code does
not understand the new columns. Run the paired down migration first, confirm
the warehouse accepts a test record, and only then promote the old tag.
Record the rollback in the incident channel with the tag you left and the tag
you returned to, so the next release starts from an accurate history.
