------------------------ MODULE ConversationTree ------------------------
EXTENDS Naturals, Sequences, FiniteSets
CONSTANTS MaxNodes, MaxTransactions, Fault
Sessions == {1, 2}
VARIABLES parents, heads, exists, revision, pending, running, preview,
          published, committedHeads, forkOrigin, forkCount, immutable, sourceStable, known
vars == <<parents, heads, exists, revision, pending, running, preview,
          published, committedHeads, forkOrigin, forkCount, immutable, sourceStable, known>>
RECURSIVE Ancestors(_, _, _)
Ancestors(ps, n, fuel) == IF n = 0 \/ fuel = 0 THEN {} ELSE
    {n} \cup Ancestors(ps, ps[n], fuel - 1)
Path(n) == Ancestors(parents, n, Len(parents))
Init == /\ parents = <<0, 1>> /\ heads = [s \in Sessions |-> 2]
        /\ exists = {1} /\ revision = 0 /\ pending = [kind |-> "none", session |-> 1, target |-> 0, expected |-> 0]
        /\ running = {} /\ preview = 0 /\ published = heads
        /\ committedHeads = {heads} /\ forkOrigin = {} /\ forkCount = 0
        /\ immutable = TRUE /\ sourceStable = TRUE
        /\ known = [s \in Sessions |-> IF s = 1 THEN {1, 2} ELSE {}]
Begin(kind, s, target) ==
    /\ pending.kind = "none" /\ revision < MaxTransactions /\ s \in exists
    /\ running = {} /\ target \in 0..Len(parents)
    /\ target \in (known[s] \cup {0})
    /\ kind \in {"edit", "select", "fork"}
    /\ (kind = "edit" => Len(parents) < MaxNodes)
    /\ (kind = "fork" => s = 1)
    /\ (kind = "fork" /\ 2 \in exists => Path(target) = forkOrigin)
    /\ pending' = [kind |-> kind, session |-> s, target |-> target, expected |-> revision]
    /\ published' = IF Fault = "early-publish" /\ kind = "edit"
          THEN [heads EXCEPT ![s] = Len(parents) + 1] ELSE published
    /\ UNCHANGED <<parents, heads, exists, revision, running, preview,
                   committedHeads, forkOrigin, forkCount, immutable, sourceStable, known>>
Commit ==
    /\ pending.kind # "none" /\ pending.expected = revision /\ running = {}
    /\ LET s == pending.session
           kind == pending.kind
           target == pending.target
           nextParents == IF kind = "edit" THEN Append(parents,
               IF Fault = "bad-parent" THEN Len(parents) + 1 ELSE target) ELSE parents
           nextHeads == IF kind = "edit" THEN [heads EXCEPT ![s] = Len(parents) + 1]
               ELSE IF kind = "select" THEN [heads EXCEPT ![s] = target]
               ELSE IF 2 \notin exists THEN [heads EXCEPT ![2] = target] ELSE heads
       IN /\ parents' = nextParents /\ heads' = nextHeads
          /\ known' = IF kind = "edit" THEN [known EXCEPT ![s] = @ \cup {Len(parents) + 1}]
               ELSE IF kind = "fork" /\ 2 \notin exists THEN [known EXCEPT ![2] = Path(target)] ELSE known
          /\ exists' = IF kind = "fork" THEN exists \cup {2} ELSE exists
          /\ forkOrigin' = IF kind = "fork" /\ 2 \notin exists THEN Path(target) ELSE forkOrigin
          /\ forkCount' = IF kind = "fork" /\ (2 \notin exists \/ Fault = "duplicate-fork")
               THEN forkCount + 1 ELSE forkCount
          /\ immutable' = immutable /\ SubSeq(nextParents, 1, Len(parents)) = parents
          /\ sourceStable' = sourceStable /\ (kind # "fork" \/ nextHeads[1] = heads[1])
          /\ committedHeads' = committedHeads \cup {nextHeads}
    /\ revision' = revision + 1 /\ pending' = [kind |-> "none", session |-> 1, target |-> 0, expected |-> 0]
    /\ UNCHANGED <<published, running, preview>>
Receive == /\ published' = heads
           /\ UNCHANGED <<parents, heads, exists, revision, pending, running, preview,
                          committedHeads, forkOrigin, forkCount, immutable, sourceStable, known>>
Crash == /\ pending' = [kind |-> "none", session |-> 1, target |-> 0, expected |-> 0] /\ published' = heads /\ running' = {}
         /\ UNCHANGED <<parents, heads, exists, revision, preview,
                        committedHeads, forkOrigin, forkCount, immutable, sourceStable, known>>
Browse(n) == /\ n \in 0..Len(parents) /\ preview' = n
             /\ UNCHANGED <<parents, heads, exists, revision, pending, running,
                            published, committedHeads, forkOrigin, forkCount, immutable, sourceStable, known>>
Run(s) == /\ pending.kind = "none" /\ s \in exists
          /\ running' = IF s \in running THEN running \ {s} ELSE running \cup {s}
          /\ UNCHANGED <<parents, heads, exists, revision, pending, preview,
                         published, committedHeads, forkOrigin, forkCount, immutable, sourceStable, known>>
Next == (\E kind \in {"edit", "select", "fork"}, s \in Sessions, n \in 0..Len(parents) : Begin(kind,s,n))
        \/ Commit \/ Receive \/ Crash \/ (\E n \in 0..Len(parents) : Browse(n))
        \/ (\E s \in Sessions : Run(s))
TypeOK == /\ parents \in Seq(0..MaxNodes) /\ Len(parents) <= MaxNodes
          /\ heads \in [Sessions -> 0..Len(parents)] /\ exists \subseteq Sessions
          /\ revision \in 0..MaxTransactions /\ running \subseteq exists
OwnedPath == \A s \in exists : Path(heads[s]) \subseteq known[s]
Acyclic == \A n \in 1..Len(parents) : parents[n] < n
AppendOnly == immutable
CommittedVisibility == published \in committedHeads
ForkAtomic == (2 \in exists) = (forkCount > 0)
ForkIdempotent == forkCount <= 1
SourceUnchangedByFork == sourceStable
Context(s) == IF Fault = "sibling-leak" THEN 1..Len(parents) ELSE Path(heads[s])
PathIsolation == \A s \in exists : Context(s) = Path(heads[s])
NoEditReachable == Len(parents) = 2
NoForkReachable == forkCount = 0
Spec == Init /\ [][Next]_vars
=============================================================================
