---
name: tdd
description: Test-driven development workflow - write the failing test first, then minimal code, then refactor
allowed-tools: read, write, edit, bash
---

# TDD - Test-Driven Development

Follow the RED-GREEN-REFACTOR cycle for every behavior change:

## The cycle

1. **RED** - Write a failing test that describes the desired behavior. Run it; watch it fail for the right reason. If it passes already, the test proves nothing - rewrite it.
2. **GREEN** - Write the minimal code that makes the test pass. No more. Resist the urge to generalize.
3. **REFACTOR** - Clean up the code you just wrote while the tests stay green. Run the full suite after every refactor step.

## Rules

- One behavior per test. Name tests after the behavior: `rejects_empty_name`, not `test_1`.
- Never write production code without a failing test first - no exceptions for "trivial" changes.
- When a bug is reported: reproduce it with a failing test BEFORE fixing, then fix, then keep the test as a regression guard.
- If a test is hard to write, the design is wrong. Fix the design, not the test.
- Keep tests deterministic: no network, no wall-clock sleeps, no randomness without a seed.

## Definition of done

- New behavior has a failing-then-passing test.
- Full suite passes (`cargo test`, `pytest`, etc. - whatever the repo uses).
- No test was weakened to make the suite pass (no deleted assertions, no loosened bounds).
