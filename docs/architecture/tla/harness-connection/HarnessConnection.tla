-------------------------- MODULE HarnessConnection --------------------------
EXTENDS Naturals, Sequences, FiniteSets
CONSTANT Fault
Ids == 1..2
VARIABLES durable, executions, acknowledged, current, phase,
          revision, connected, seen, queue, lagged, status, resources, stopRequested
vars == <<durable, executions, acknowledged, current, phase,
          revision, connected, seen, queue, lagged, status, resources, stopRequested>>
Init == /\ durable = [i \in Ids |-> "absent"] /\ executions = [i \in Ids |-> 0]
        /\ acknowledged = {} /\ current = 0 /\ phase = "idle"
        /\ revision = 0 /\ connected = FALSE /\ seen = 0 /\ queue = <<>> /\ lagged = FALSE
        /\ status = "running" /\ resources = TRUE /\ stopRequested = FALSE
Reserve(i) == /\ phase = "idle"
              /\ (durable[i] = "absent" \/ (Fault = "retry" /\ durable[i] = "reserved"))
              /\ durable' = [durable EXCEPT ![i] = "reserved"] /\ current' = i /\ phase' = "reserved"
              /\ UNCHANGED <<executions, acknowledged, revision, connected, seen, queue, lagged, status, resources, stopRequested>>
Execute == /\ phase = "reserved" /\ executions[current] < 2
           /\ executions' = [executions EXCEPT ![current] = @ + 1] /\ phase' = "executed"
           /\ UNCHANGED <<durable, acknowledged, current, revision, connected, seen, queue, lagged, status, resources, stopRequested>>
Complete == /\ phase = "executed" /\ durable' = [durable EXCEPT ![current] = "done"] /\ phase' = "receipt"
            /\ UNCHANGED <<executions, acknowledged, current, revision, connected, seen, queue, lagged, status, resources, stopRequested>>
Acknowledge == /\ (phase = "receipt" \/ (Fault = "early-ack" /\ phase = "executed"))
               /\ acknowledged' = acknowledged \cup {current} /\ phase' = "idle" /\ current' = 0
               /\ UNCHANGED <<durable, executions, revision, connected, seen, queue, lagged, status, resources, stopRequested>>
Crash == /\ phase # "idle" /\ phase' = "idle" /\ current' = 0
         /\ UNCHANGED <<durable, executions, acknowledged, revision, connected, seen, queue, lagged, status, resources, stopRequested>>
Connect == /\ (~connected \/ lagged) /\ connected' = TRUE
           /\ seen' = IF Fault = "snapshot-gap" /\ revision > 0 THEN revision - 1 ELSE revision
           /\ queue' = <<>> /\ lagged' = FALSE
           /\ UNCHANGED <<durable, executions, acknowledged, current, phase, revision, status, resources, stopRequested>>
Publish == /\ revision < 2 /\ revision' = revision + 1
           /\ queue' = IF connected /\ ~lagged /\ Len(queue) < 1 THEN Append(queue, revision') ELSE queue
           /\ lagged' = (lagged \/ (connected /\ Len(queue) = 1 /\ Fault # "silent-drop"))
           /\ UNCHANGED <<durable, executions, acknowledged, current, phase, connected, seen, status, resources, stopRequested>>
Consume == /\ connected /\ ~lagged /\ Len(queue) > 0 /\ seen' = Head(queue) /\ queue' = Tail(queue)
           /\ UNCHANGED <<durable, executions, acknowledged, current, phase, revision, connected, lagged, status, resources, stopRequested>>
Disconnect == /\ connected /\ connected' = FALSE /\ queue' = <<>>
              /\ status' = IF Fault = "disconnect-stops" THEN "settling" ELSE status
              /\ UNCHANGED <<durable, executions, acknowledged, current, phase, revision, seen, lagged, resources, stopRequested>>
Stop == /\ status = "running" /\ status' = "settling" /\ stopRequested' = TRUE
        /\ UNCHANGED <<durable, executions, acknowledged, current, phase, revision, connected, seen, queue, lagged, resources>>
Join == /\ status = "settling" /\ resources /\ resources' = FALSE
        /\ UNCHANGED <<durable, executions, acknowledged, current, phase, revision, connected, seen, queue, lagged, status, stopRequested>>
Ready == /\ status = "settling" /\ (~resources \/ Fault = "early-ready") /\ status' = "ready"
         /\ UNCHANGED <<durable, executions, acknowledged, current, phase, revision, connected, seen, queue, lagged, resources, stopRequested>>
Next == (\E i \in Ids : Reserve(i)) \/ Execute \/ Complete \/ Acknowledge \/ Crash \/ Connect \/ Publish \/ Consume \/ Disconnect \/ Stop \/ Join \/ Ready
Spec == Init /\ [][Next]_vars
AtMostOnce == \A i \in Ids : executions[i] <= 1
ConfirmedAfterDurable == \A i \in acknowledged : durable[i] = "done"
NoSilentGap == connected /\ ~lagged => seen + Len(queue) = revision
ReadyAfterJoin == status = "ready" => ~resources
ObserverIsolation == status # "running" => stopRequested
=============================================================================
