# Testing 05: fortsh-Scale and Large-Program Campaigns

## Goal

Apply the shared testing language and the structured harness strategy to larger
program campaigns, culminating in fortsh-scale work.

## Focus Areas

### Large-program campaigns

Scale beyond tiny fixtures while preserving debuggability:

- larger single-file programs
- multi-file/module graph programs
- imported fixtures that represent historical compiler bugs
- fortsh-derived reductions where licensing and maintenance allow

### Heavy-lane ownership

This is where `bencch` should do more of the lifting:

- large opt matrices
- reference/differential campaigns
- graph orchestration
- failure bundles and reporting
- capability-aware execution plans

### Campaign design

Campaigns should not just ask "does it compile?" They should ask:

- does the runtime behavior remain correct?
- do expected diagnostics stay correct?
- do graph/module invariants hold?
- does behavior stay stable across opt levels?
- do known unsupported surfaces report as unsupported, not broken?

## fortsh Relationship

fortsh remains the largest real-world acceptance target, but this sprint treats
it as one campaign among several. The testing system should still grow in areas
fortsh does not naturally cover.

## Acceptance Scenarios

- one large multi-file/module graph campaign
- one differential campaign on a larger real-program subset
- one fortsh-adjacent reduction family integrated into the structured runner
- one documented heavy-lane workflow for triaging failures back into smaller
  source-directed tests
