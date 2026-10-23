# The Second Brain's folder is a private git repository managed by Agents

The Second Brain's files live in their own git repository, separate from this project's source and pushed to a private remote. Agents, not the owner, make the commits: every change gets a generated message carrying identifiers (Recollection, Repository, Branch) so history can be searched and any removal reversed. This gives reversible pruning, backup and cross-machine sync without building our own versioning, at the cost of treating git as part of the storage layer.
