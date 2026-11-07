# Hardware Detection and Runtime Management

Status: **proposed, awaiting agreement.** Machine facts below were observed read-only on 2026-11-06.
No benchmark has been run, and no performance figures appear in this document.

## What was found on the development machine

| Item | Observed |
| --- | --- |
| OS | Fedora 44 KDE, kernel 7.2.13, Wayland, SELinux enforcing |
| CPU | Ryzen 9 5950X, 16 cores / 32 threads, AVX2 and FMA, no AVX-512 |
| RAM | 93 GiB visible, about 74 GiB available, with 3.7 GiB of swap in use |
| GPU | RX 6900 XT (Navi 21, `1002:73af`), `amdgpu` driver |
| VRAM | 15.98 GiB total, **about 1.9 GiB already in use with no model loaded** |
| Compute | `/dev/kfd` present. `rocminfo`, Vulkan (RADV) and `clinfo` available. `llvmpipe` also listed as a Vulkan device. |
| Disk | 532 GiB free on `/home` |
| Ollama | `ollama.service`, generated from `~/.config/containers/systemd/ollama.container`, **inactive** |
| Pod | `local-ai`, publishing 3000, 11434 and 8880. Shared with Open WebUI. |
| Ollama image | `docker.io/ollama/ollama:rocm` |
| Ollama env | `OLLAMA_CONTEXT_LENGTH=32768`, `OLLAMA_KV_CACHE_TYPE=q8_0`, `OLLAMA_FLASH_ATTENTION=1`, `OLLAMA_MAX_LOADED_MODELS=4`, `OLLAMA_NUM_PARALLEL=1`, `HSA_OVERRIDE_GFX_VERSION=10.3.0` |
| Other runtimes | Nothing answering on 1234, 8000, 8080 or 3000 |

The API lists 14 installed models. 8 were measured for tool-calling, speed and memory: see the [spike report](spikes/2026-11-06-ollama.md).

### Lessons for the design

1. **The spec's starting assumption was out of date within the day.** Discovery can't assume a running
   container. It has to report **running**, **installed but stopped**, or **absent**.
2. **`podman ps -a` isn't sufficient.** Quadlet containers are removed when stopped (the journal shows
   `container remove`), so the stopped service is only visible through systemd units and Quadlet files.
3. **The 27B models show why "fits in VRAM" is the wrong test.** Their 15.7 GiB of weights looks like
   it fits in a 16 GB card. But there's about 2 GiB of desktop usage, and KV cache on top. Measured at
   `num_ctx=16384`, `qwen3.6:27b-coding` loaded 55 of 66 layers on the GPU, with 13.52 of 16.98 GiB in VRAM.
4. **Free VRAM sources disagree.** Ollama logged `free="15.9 GiB"`, while sysfs shows 1.9 GiB in use
   today. These were at different times, so desktop usage may simply have differed, but we need to
   find out which figure predicts real loading behaviour.
5. **Shared service.** Peak memory in the last run was 66.9 GiB, and `OLLAMA_NUM_PARALLEL=1` means one
   active slot. Any background request to the same model can displace the prompt cache.

### Never parse Ollama's on-disk layout

An earlier version of this document warned that 80.2 GiB of blobs were unreferenced and might be pruned
at startup. **That was wrong.** The scan read only `models/manifests/`, while Ollama 0.40.14 keeps most
models in `models/manifests-v2/`. The API lists 14 models, and on startup the server logged
`total blobs: 69` and `total unused blobs removed: 0`.

Product requirements that follow:

- **Use the API as the source of truth for models.** With the server stopped, say that the model list is
  unavailable or partial. Never draw conclusions about storage from the files.
- Never start a user-managed runtime without saying exactly what will happen.

## Hardware detection

Detection is a set of independent probes. A failed probe gives "unknown" and never aborts or guesses.

| Data | Source |
| --- | --- |
| CPU, instruction sets | `/proc/cpuinfo`, `sysinfo` |
| Memory | `/proc/meminfo`. Use `MemAvailable`, not total. |
| AMD GPU | `/sys/class/drm/card*/device`: `mem_info_vram_total`, `mem_info_vram_used`, `gpu_busy_percent`, driver link |
| NVIDIA GPU | NVML via dlopen (`nvml-wrapper`), falling back to `nvidia-smi`. **Untested, no NVIDIA hardware here.** |
| Intel GPU | sysfs/i915 or xe. **Untested, no Intel GPU here.** |
| Compute runtimes | `/dev/kfd`, `rocminfo`, `vulkaninfo`, `clinfo` if present. Filter software devices like `llvmpipe`. |
| Containers | `podman`, then `docker`, with GPU device access checked separately |
| Disk | statvfs on the actual model storage path, not `$HOME` |

**The GPU a host can see isn't always available to a container,** so "ROCm works in the host" doesn't
imply it works for a containerised runtime. The Ollama journal is better evidence here: it logs
`library=ROCm` and GPU layer offload, confirming ROCm worked in this container on Nov 5.

Platform-specific probes go behind one interface so Windows and Apple Silicon can add theirs later.

## Runtime discovery

**Implemented 2026-11-07 in `crates/runtime`** (`discover()`), with these refinements learnt on the
development machine: Ollama is identified by the plain-text greeting at `/` ("Ollama is running"), because
Open WebUI answers `/api/version` in the same shape; a running container with a known image is probed on
the ports it publishes, not only the usual ones; a process whose cgroup names a `libpod`/`docker` scope
belongs to that container and isn't a second runtime; podman 5 leaves `PodName` empty in `ps`, so pods come
from `pod ps`; Quadlet `AddDevice=` shows up only in `Config.CreateCommand`, not `HostConfig.Devices`.

Runs in order. All steps are read-only:

1. Probe configured URLs, then known ports. Identify from the response, not the port number.
2. Check systemd user and system units, including Quadlet-generated ones.
3. Inspect containers for known images, without assuming their names.
4. Look for native binaries on `PATH`.
5. Read models through the API if running. Otherwise read manifests from disk, labelled as an offline view.

```rust
enum Ownership { UserManaged, AppManaged }
enum RuntimeState {
    Running { url: Url, version: String },
    InstalledStopped { how_to_start: StartMethod },
    Unreachable { url: Url, error: String },
    Absent,
}
```

## Ownership rules

| Action | User-managed | App-managed |
| --- | --- | --- |
| Connect and read status | Yes | Yes |
| Use for inference | Yes | Yes |
| Start | **Ask each time**, naming the exact unit, or remember if the user opts in | Automatic |
| Stop, restart | **Never automatically.** Explicit request only. | Automatic, respecting other sessions |
| Change config, env or image | **Never.** Show the advice instead. | Yes |
| Pull a model | Ask, showing size and destination | Ask, showing size and destination |
| Delete a model | **Never** except on explicit, confirmed request | Confirmed |

Per-request parameters such as `num_ctx` are normal API use and aren't config changes, but their
effects are visible to anything sharing the runtime, such as a model reload for Open WebUI. The UI
should surface this when a session changes them.

**One shared service.** Sessions reuse a runtime through a reference-counted registry instead of
launching duplicates. An isolated instance requires an explicit configuration.

## App-managed runtimes (Milestone 3)

- **Podman:** rootless, using Quadlet units in the user's systemd directory, namespaced with a
  `re-code-` prefix so ownership is unambiguous. Docker is optional, and containers aren't mandatory.
- **Native:** process supervision for binaries such as `llama-server`.
- **Downloads:** verified with published digests when available, resumable, and always showing size,
  destination and licence. **Never execute a downloaded install script silently.**

## Memory estimation

The estimate is a sum of separate parts. It's shown as an estimate until replaced by measurement:

```text
required  = weights
          + KV cache      ≈ n_ctx × n_layers × per-token bytes per layer (depends on architecture,
                            head dimensions, parallel slots, and cache type)
          + runtime overhead and graph scratch
          + vision encoder or other components, if any

available = VRAM total − VRAM currently used − safety margin
```

Metadata comes from GGUF headers and `/api/show`. Formula coefficients will be calibrated against real
loads on this machine, using `/api/ps` (`size` and `size_vram`) and journal output.

**Measured data outranks the estimate.** After a real load, the system stores the actual memory used
and layer split for that model, context size and runtime. It's also worth noting that this server's
`OLLAMA_KV_CACHE_TYPE=q8_0` could shift KV cache size, so the model must read this setting if available.

## Recommendations

Each candidate gets one of these plain-language outcomes:

| Label | Meaning | Example wording |
| --- | --- | --- |
| **Recommended** | Fully on GPU with a usable context | "Should give a good balance of speed and memory use on your computer." |
| **Works, slower** | Partly on CPU | "Will run partly on your CPU, so expect slower responses." |
| **Advanced** | Requires tradeoffs, or is close to a limit | "Needs a shorter context length, or CPU offloading." |
| **Cloud** | Not realistic locally | "Your hardware may struggle with this. A hosted version is available." |

A coding agent also needs a **usable context**, and that's a recommendation criterion: a model that
only fits at 4k context isn't suitable for agent work, whatever its weight size.

Every recommendation has a one-key explanation showing the arithmetic. Estimated and measured
performance are always marked separately.

The catalogue of recommendable models is versioned data, updated independently of code.

## Benchmarks (Milestone 3)

Optional and started by the user. Measures model load time, prompt evaluation and generation speed,
actual memory, GPU utilisation, and throughput as context grows. Conformance probe results count as
the tool-reliability measure.

Results record the model digest, runtime version, machine and date. **Figures shown are always real
measurements from this machine, and we don't ship or display invented numbers.**
