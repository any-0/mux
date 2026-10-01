# Raw baseline reproductions

Both cases were executed on main `d6dd228054231e77772bd17a412d8f0d07871835`
using the locked project Nix toolchain in run
https://github.com/any-0/mux/actions/runs/36748241151.
`provenance.json` records the separate harness commit and original artifact.
Only isolated test paths/profile data occur in these traces.

`baseline-intensity` includes the actual inner pane bytes and client output.
The following deterministic replay must report a failure on four cells:

```sh
./scripts/stress-nix python3 stress/replay.py stress/evidence/baseline-intensity
```

`baseline-sidebar` ran the shell directly, created ten windows, and expected
all four cells in the process tile to carry the explicitly configured tile
background. `witness-result.json` records the failing coordinate/color.
To reproduce live, run `stress/witnesses.py --case sidebar` with a separately
built baseline binary as documented in `docs/terminal-stress.md`.

Keep these source traces unchanged. Fixed behavior is validated by running the
same witness against the candidate binary, not by editing captured output.
