--------------------------- MODULE HtmlPreview ---------------------------
EXTENDS Naturals, FiniteSets
CONSTANT Fault
Pages == {"a", "b"}
Sessions == {"A", "B"}
Sites == {"inline", "sidebar"}
VARIABLES session, version, live, visible, covered, displayed, pending, measured, expanded,
          hosts, ready, pendingReady, lateReady
vars == <<session, version, live, visible, covered, displayed, pending, measured, expanded,
          hosts, ready, pendingReady, lateReady>>
Token(p, site) == [session |-> session, page |-> p, version |-> version[p], site |-> site]
Host(t) == [session |-> t.session, page |-> t.page, site |-> t.site]
Mounted(next, large) == {Token(p, "inline") : p \in next}
                       \cup IF large = "none" THEN {} ELSE {Token(large, "sidebar")}
\* A selected sidebar prepares from the canonical source without an inline mount.
Loaded(next, large) == IF Fault = "sidebar-source" /\ large # "none" /\ large \notin next
                       THEN {Token(p, "inline") : p \in next}
                            \cup {[Token(large, "sidebar") EXCEPT !.version = 0]}
                       ELSE Mounted(next, large)
Init == /\ session = "A" /\ version = [p \in Pages |-> 0]
        /\ live = {} /\ visible = {} /\ covered = FALSE
        /\ displayed = {} /\ pending = {} /\ measured = {} /\ expanded = "none"
        /\ hosts = {} /\ ready = {} /\ pendingReady = {} /\ lateReady = FALSE
Frame(next, cover, large) ==
    /\ expanded' = large
    /\ visible' = next /\ covered' = cover
    /\ live' = {t \in live : t.site = "inline"} \cup Loaded(next, large)
    /\ displayed' = IF cover THEN {}
                      ELSE IF Fault = "exclusive" /\ large # "none"
                           THEN Mounted(next \ {large}, large)
                      ELSE Loaded(next, large)
    /\ measured' = {t \in measured : t.site = "inline" \/ t.page = large}
    /\ hosts' = {h \in hosts : h.site = "inline" \/ h.page = large}
                 \cup {Host(t) : t \in Mounted(next, large)}
    /\ ready' = ready \cap hosts'
    /\ UNCHANGED <<session, version, pending, pendingReady, lateReady>>
Queue(t) ==
    /\ t \in live /\ Cardinality(pending) < 2 /\ pending' = pending \cup {t}
    /\ UNCHANGED <<session, version, live, visible, covered, displayed, measured, expanded,
                   hosts, ready, pendingReady, lateReady>>
Receive(t) ==
    /\ t \in pending /\ pending' = pending \ {t}
    /\ measured' = IF Fault = "stale" \/ (t = Token(t.page, t.site) /\ t \in live)
                    THEN {m \in measured : m.page # t.page \/ m.site # t.site} \cup {t} ELSE measured
    /\ UNCHANGED <<session, version, live, visible, covered, displayed, expanded,
                   hosts, ready, pendingReady, lateReady>>
QueueReady(t) ==
    /\ t \in live /\ t.page = "a" /\ t.site = "inline"
    /\ Host(t) \notin ready /\ pendingReady = {}
    /\ pendingReady' = {t}
    /\ UNCHANGED <<session, version, live, visible, covered, displayed, pending, measured,
                   expanded, hosts, ready, lateReady>>
ReceiveReady(t) ==
    /\ t \in pendingReady /\ pendingReady' = {}
    /\ ready' = IF Host(t) \in hosts \/ Fault = "stale-ready" THEN ready \cup {Host(t)} ELSE ready
    /\ lateReady' = (lateReady \/ (Host(t) \in hosts /\ t.version # version[t.page]))
    /\ UNCHANGED <<session, version, live, visible, covered, displayed, pending, measured,
                   expanded, hosts>>
Rewrite(p) ==
    /\ version[p] = 0 /\ version' = [version EXCEPT ![p] = 1]
    /\ live' = {t \in live : t.page # p}
    /\ displayed' = {t \in displayed : t.page # p}
    /\ measured' = {t \in measured : t.page # p}
    /\ UNCHANGED <<session, visible, covered, pending, expanded, hosts, ready, pendingReady, lateReady>>
Switch ==
    /\ session = "A" /\ session' = "B" /\ expanded' = "none"
    /\ live' = {} /\ visible' = {} /\ displayed' = {} /\ measured' = {}
    /\ hosts' = {} /\ ready' = {}
    /\ UNCHANGED <<version, covered, pending, pendingReady, lateReady>>
Next == (\E next \in SUBSET Pages : \E cover \in BOOLEAN : \E large \in Pages \cup {"none"} : Frame(next, cover, large))
        \/ (\E p \in Pages : Rewrite(p))
        \/ (\E t \in live : Queue(t)) \/ (\E t \in pending : Receive(t)) \/ Switch
        \/ (\E t \in live : QueueReady(t)) \/ (\E t \in pendingReady : ReceiveReady(t))
Spec == Init /\ [][Next]_vars
CurrentOnly == \A t \in live \cup displayed \cup measured : t = Token(t.page, t.site)
ReadyOnly == ready \subseteq hosts
StreamingReadyReachable == ~lateReady
ClippedVisibility == displayed \subseteq Mounted(visible, expanded) /\ (covered => displayed = {})
MountedDocumentsVisible == ~covered => live \cap Mounted(visible, expanded) \subseteq displayed
HiddenStateRetained == [][(UNCHANGED <<session, version>>) => {t \in live : t.site = "inline"} \subseteq live']_vars
MultiplePreviewsReachable == Cardinality(displayed) < 2
SidebarAndInlineReachable == ~(\E p \in Pages : Token(p, "inline") \in displayed /\ Token(p, "sidebar") \in displayed)
UpdatedSidebarAloneReachable == ~(\E p \in Pages : version[p] = 1 /\ Token(p, "sidebar") \in displayed /\ p \notin visible)
=============================================================================
