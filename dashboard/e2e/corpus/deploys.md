# Deploy checklist

Run the local CI script before pushing. Tag nothing: a version bump merged to
main is the release. Roll back by reverting the merge commit.

## Rollback

Revert the merge, wait for the release job, and confirm the health endpoint.
