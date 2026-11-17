# Fixed code tasks

Run the deterministic, offline harness with:

```sh
cargo test -p mycode --test code_tasks --locked
```

The five named tasks execute the actual agent loop and native file tools. CT02 also runs QuickJS CodeMode; CT05 runs a child agent and returns its retained result through the real `agent` tool. Scripted providers choose the calls and inspect prior real tool results. They never call a model API.

| Task | Fixed input and acceptance |
| --- | --- |
| CT01 | Repair percentage bounds. The Rust oracle checks zero, interior, boundary, overflow-range inputs, and the unrelated label function. |
| CT02 | Find and repair two invoice rules in separate files. Zero units are free; the delivery fee begins strictly above ten units. CodeMode reads, edits, and searches the files. |
| CT03 | Recover after an ambiguous edit is rejected. Only `previous` changes; the independent oracle checks both `previous` and `next` over negative, zero, and positive inputs. |
| CT04 | Reject an unknown tool and a malformed write without changing the input, then complete CT01 using valid calls. |
| CT05 | Delegate CT01, return a usable run ID and result artifact, and inspect the actual changed source in the parent. Parent and child share the request budget. |

Each run copies immutable inputs into a unique OS temporary directory. The oracle remains outside the candidate workspace. It compiles the resulting code with `rustc --test`, executes its behavioral checks, and also checks that unrelated files retain their exact bytes and no files were added or removed. The harness cancels leftover CodeMode work, waits for native workers and child handoffs, and kills and reaps a timed-out compiler or oracle. Temporary files and binaries are removed after their last owner finishes; a detached driver retains its directory until it settles. Rust and its native linker must be installed.

These offline results establish deterministic tool/runtime behavior, **not model reasoning quality or a model task-success rate**. A scripted answer is never an acceptance oracle. There is no learned judge, paid request, model ranking, or timing claim in the default suite.

## Opt-in live model evaluation

The ignored test uses the current MYCode Magpie binding from local settings, including `MYCODE_HOME` when configured. It does not print the gateway key, headers, or raw provider errors. No SDK or new dependency is required.

PowerShell:

```powershell
$env:MYCODE_EVAL_LIVE = "1"
$env:MYCODE_EVAL_TASK = "ct01" # Optional: remove to run all five tasks.
cargo test -p mycode --test code_tasks --locked live_magpie_fixed_code_tasks -- --ignored --nocapture
Remove-Item Env:MYCODE_EVAL_LIVE
Remove-Item Env:MYCODE_EVAL_TASK -ErrorAction SilentlyContinue
```

POSIX shell:

```sh
MYCODE_EVAL_LIVE=1 MYCODE_EVAL_TASK=ct01 cargo test -p mycode --test code_tasks --locked live_magpie_fixed_code_tasks -- --ignored --nocapture
```

Both `--ignored` and the explicit environment opt-in are required. Live requests can consume paid quota. Each task has a shared allowance of 16 agent model requests, requests an 8192-token output cap per response, and stops agent work after 300 seconds. Cleanup and independent oracle execution are outside that deadline. Provider-internal transport retries retain their normal behavior and are not separate agent budget slots. A full run contains five tasks; CT05 counts both the parent and child against the same allowance.

Live CT04 begins with a recorded rejected write in its conversation and must repair the unchanged fixture. It does not instruct the model to invent tools. CT03 describes the earlier ambiguous attempt; the offline case separately proves the runtime rejection. Live CT05 requires a real child handoff. The same file preservation and compiled behavioral oracles grade all live cases. The candidate has only fixture-scoped read/edit/write/search tools and CodeMode; CT05 additionally exposes `agent`. The compiled candidate program is not an OS sandbox.

Output names the task, selected model, PASS/FAIL, and requests used. Any failed task causes a nonzero test exit status. Keep task inputs and oracles fixed when comparing models or prompts; report the exact commit, model, configuration, per-task outcomes, and number of repeated runs. Five small tasks are a smoke benchmark, not evidence about general coding ability, cost savings, or security.
