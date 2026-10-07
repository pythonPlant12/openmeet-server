# OpenMeet patch to webrtc-ice 0.17.1

Upstream webrtc-ice (0.17.1, and 0.17.2 at the time of writing) drops an ICE check that conflicts
with its own role instead of resolving the conflict. RFC 8445 section 7.3.1.1 requires resolving it.
Current Chrome resolves role conflicts by switching roles. After the SFU sends a renegotiation offer,
the SFU and Chrome could end up in the same role. Neither side then nominated a pair, consent checks
stopped, and calls failed with "Connection failed" when another participant joined.

The only change is in `src/agent/agent_internal.rs`. Every change there is marked with
`OpenMeet patch`.

- An inbound binding request whose ICE-CONTROLLING or ICE-CONTROLLED attribute conflicts with the
  local role is resolved with the tie-breaker. The agent either switches role or answers with
  `487 Role Conflict`.
- An inbound `487 Role Conflict` error response switches the local role.
- A check that carries the agent's own tie-breaker is its own check looping back, for example
  through a shared TURN relay. It is ignored as before.

When you upgrade `webrtc`, check whether upstream now handles role conflicts. If it does, delete this
directory and the `[patch.crates-io]` entry in `Cargo.toml`. If it does not, reapply the patch to the
matching webrtc-ice version.
