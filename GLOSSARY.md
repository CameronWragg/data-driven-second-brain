# Second Brain

An AI-first personal knowledge system that holds structured, linked knowledge about software engineering work and serves it to AI agents for fast retrieval.

## Language

**Second Brain**:
The whole knowledge store belonging to one owner, together with everything that indexes and serves it.
_Avoid_: Knowledge base, vault, workspace

**Latens**:
The product: the program that holds a Second Brain and serves it to Agents. One owner may install Latens on several machines, all serving the same Second Brain.
_Avoid_: brain (as the program's name), second-brain

**Agent**:
An AI client that reads from and writes to the Second Brain through its MCP interface; the primary user of the system.
_Avoid_: Bot, assistant, client

**Entity**:
A thing that exists in the world independently of the Second Brain, such as a Repository or a Dependency.
_Avoid_: Node, object, resource

**Recollection**:
A unit of distilled knowledge about one or more Entities, such as a decision, convention or gotcha.
_Avoid_: Note, memory, document, entry, file

**Item**:
A Recollection or an Entity: anything in the Second Brain that has an ID and a file of its own.
_Avoid_: Record, object, file

**Kind**:
The category of a Recollection: decision, convention, caveat, architecture or how-to. Where results list Items, an Entity's kind is `repository` or `dependency`, which are not Kinds.
_Avoid_: Type, category, gotcha, pitfall

**Anchor**:
A pointer from a Recollection to a precise region of code: a Repository, a path, a span of lines within it, and the commit it was written against.
_Avoid_: Reference, pin, location

**Stale**:
Describes a Recollection whose Anchored code has changed since it was written; a warning raised when it is read, not a stored state.
_Avoid_: Outdated, expired, invalid

**Source**:
Something that feeds Entities, Links or Recollections into the Second Brain through a standard interface.
_Avoid_: Plugin, ingestor, connector, integration

### Software engineering

**Repository**:
A codebase the owner works on, identified independently of where it happens to be checked out.
_Avoid_: Repo, project, codebase

**Checkout**:
A local copy of a Repository on a particular machine and path.
_Avoid_: Clone, working copy, workspace

**Branch**:
A line of work within a Repository; Recollections written on it belong to it until it is merged or abandoned.
_Avoid_: Ref, feature branch

**Promotion**:
Moving a merged Branch's Recollections into the Branch it was merged into, reconciled against what is already known there.
_Avoid_: Merge, migration, copy

**Dependency**:
An external package that one or more Repositories use; knowledge about it is shared across all of them.
_Avoid_: Library, package, third-party

### Relationships

**Link**:
An explicit, typed relationship between two things in the Second Brain, declared by an Agent or a plugin. It is a fact.
_Avoid_: Edge, reference, relation

**Similarity**:
A computed score of how closely two things relate in meaning. It is a guess, and it is never stored as a Link.
_Avoid_: Relationship strength, relevance, closeness
