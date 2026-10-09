------------------------- MODULE HtmlPublication -------------------------
EXTENDS Naturals, FiniteSets
CONSTANT Fault
Versions == 0..2
VARIABLES generation, complete, prepared, initialized, hostReady, published, rendered, queued, counts
vars == <<generation, complete, prepared, initialized, hostReady, published, rendered, queued, counts>>
Init == /\ generation = 0 /\ complete = FALSE /\ prepared = FALSE /\ initialized = FALSE /\ hostReady = FALSE
        /\ published = 3 /\ rendered = 3 /\ queued = {}
        /\ counts = [v \in Versions |-> 0]
HostReady == /\ ~hostReady /\ hostReady' = TRUE
             /\ UNCHANGED <<generation, complete, prepared, initialized, published, rendered, queued, counts>>
Append == /\ ~complete /\ generation = 0 /\ generation' = 1 /\ prepared' = FALSE
          /\ UNCHANGED <<complete, initialized, hostReady, published, rendered, queued, counts>>
Finish == /\ ~complete /\ complete' = TRUE
          /\ UNCHANGED <<generation, prepared, initialized, hostReady, published, rendered, queued, counts>>
Prepare == /\ ~prepared /\ prepared' = TRUE
           /\ UNCHANGED <<generation, complete, initialized, hostReady, published, rendered, queued, counts>>
Initialize == /\ published # 3 /\ ~initialized /\ initialized' = TRUE
              /\ UNCHANGED <<generation, complete, prepared, hostReady, published, rendered, queued, counts>>
Publish == /\ hostReady /\ (complete \/ Fault = "partial")
           /\ (prepared \/ Fault = "unprepared")
           /\ (published = 3 \/ Fault = "repeat") /\ counts[generation] < 2
           /\ published' = generation
           /\ counts' = [counts EXCEPT ![generation] = @ + 1]
           /\ UNCHANGED <<generation, complete, prepared, initialized, hostReady, rendered, queued>>
Queue == /\ published # 3 /\ (initialized \/ Fault = "early")
         /\ queued' = queued \cup {published}
         /\ UNCHANGED <<generation, complete, prepared, initialized, hostReady, published, rendered, counts>>
Receive(v) == /\ v \in queued /\ queued' = queued \ {v}
              /\ rendered' = IF (v = generation /\ published = v) \/ Fault = "stale"
                               THEN v ELSE rendered
              /\ UNCHANGED <<generation, complete, prepared, initialized, hostReady, published, counts>>
Rewrite == /\ generation < 2 /\ complete /\ generation' = 2
           /\ complete' = FALSE /\ prepared' = FALSE /\ initialized' = FALSE /\ hostReady' = FALSE
           /\ published' = 3 /\ rendered' = 3
           /\ UNCHANGED <<queued, counts>>
Next == HostReady \/ Append \/ Finish \/ Prepare \/ Publish \/ Initialize \/ Queue \/ Rewrite
        \/ (\E v \in queued : Receive(v))
Spec == Init /\ [][Next]_vars
CompleteOnly == published # 3 => complete /\ prepared /\ published = generation
InitializedOnly == rendered # 3 => complete /\ prepared /\ initialized /\ published = generation /\ rendered = generation
AtMostOnce == \A v \in Versions : counts[v] <= 1
RenderedReachable == rendered = 3
=============================================================================
