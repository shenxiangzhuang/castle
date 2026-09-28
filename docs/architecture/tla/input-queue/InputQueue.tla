-------------------------- MODULE InputQueue --------------------------
EXTENDS Naturals, FiniteSets
CONSTANTS MessageCount, Fault
Ids == 1..MessageCount
Pending(s) == s \in {"queued", "priority"}
VARIABLES durable, visible, submitted, cancelled, counts, phase,
          running, boundary, pausedCounts
vars == <<durable, visible, submitted, cancelled, counts, phase,
          running, boundary, pausedCounts>>
Init ==
    /\ durable = [i \in Ids |-> "absent"] /\ visible = durable
    /\ submitted = {} /\ cancelled = {} /\ counts = [i \in Ids |-> 0]
    /\ phase = "ready" /\ running = TRUE /\ boundary = "busy"
    /\ pausedCounts = counts
Ready == phase = "ready"
Commit(i, state) ==
    /\ durable' = [durable EXCEPT ![i] = state]
    /\ phase' = "receipt" /\ UNCHANGED visible
Submit(i) ==
    /\ Ready /\ running /\ durable[i] = "absent" /\ i \notin submitted
    /\ Commit(i, "queued") /\ submitted' = submitted \cup {i}
    /\ UNCHANGED <<cancelled, counts, running, boundary, pausedCounts>>
Prioritize(i) ==
    /\ Ready /\ running /\ Pending(durable[i])
    /\ Commit(i, "priority")
    /\ UNCHANGED <<submitted, cancelled, counts, running, boundary, pausedCounts>>
Cancel(i) ==
    /\ Ready /\ Pending(durable[i])
    /\ Commit(i, IF Fault = "forget" THEN "absent" ELSE "cancelled")
    /\ cancelled' = cancelled \cup {i}
    /\ UNCHANGED <<submitted, counts, running, boundary, pausedCounts>>
Receive ==
    /\ phase = "receipt" /\ visible' = durable /\ phase' = "ready"
    /\ UNCHANGED <<durable, submitted, cancelled, counts, running, boundary, pausedCounts>>
Finish(kind) ==
    /\ running /\ boundary = "busy" /\ boundary' = kind
    /\ UNCHANGED <<durable, visible, submitted, cancelled, counts, phase, running, pausedCounts>>
Eligible(i) ==
    /\ (running \/ Fault = "after-stop")
    /\ ((durable[i] = "priority" /\ boundary \in {"step", "turn"})
        \/ (durable[i] = "queued" /\ boundary = "turn"
            /\ ~\E j \in Ids : durable[j] = "priority")
        \/ (Fault = "cancelled-attach" /\ durable[i] = "cancelled")
        \/ (Fault = "duplicate" /\ durable[i] = "attached" /\ counts[i] < 2))
    /\ ~\E j \in Ids : j < i /\ durable[j] = durable[i] /\ Pending(durable[j])
Attach(i) ==
    /\ Ready /\ Eligible(i) /\ Commit(i, "attached")
    /\ counts' = [counts EXCEPT ![i] = @ + 1] /\ boundary' = "busy"
    /\ UNCHANGED <<submitted, cancelled, running, pausedCounts>>
Stop ==
    /\ Ready /\ running /\ running' = FALSE /\ pausedCounts' = counts
    /\ UNCHANGED <<durable, visible, submitted, cancelled, counts, phase, boundary>>
Resume ==
    /\ Ready /\ ~running /\ (\E i \in Ids : Pending(durable[i]))
    /\ running' = TRUE /\ boundary' = "turn"
    /\ UNCHANGED <<durable, visible, submitted, cancelled, counts, phase, pausedCounts>>
Next == (\E i \in Ids : Submit(i) \/ Prioritize(i) \/ Cancel(i) \/ Attach(i))
        \/ Receive \/ Finish("step") \/ Finish("turn") \/ Stop \/ Resume
Spec == Init /\ [][Next]_vars
TypeOK == /\ durable \in [Ids -> {"absent", "queued", "priority", "attached", "cancelled"}]
          /\ visible \in [Ids -> {"absent", "queued", "priority", "attached", "cancelled"}]
          /\ counts \in [Ids -> 0..2] /\ phase \in {"ready", "receipt"}
NoLostInput == \A i \in submitted : durable[i] # "absent"
AtMostOnce == \A i \in Ids : counts[i] <= 1
CancelledNeverAttached == \A i \in cancelled : counts[i] = 0
StopPausesQueue == ~running => counts = pausedCounts
CommittedVisibility == /\ (phase = "ready" => visible = durable)
                       /\ (\A i \in Ids : visible[i] = "attached" => counts[i] > 0)
NoPriority == ~\E i \in Ids : durable[i] = "priority"
NoPausedPending == running \/ ~\E i \in Ids : Pending(durable[i])
=============================================================================
