# Ryujinx Crash Investigation: Second Wind Mod on BotW

## Summary

BotW + Second Wind crashes in Ryujinx during save game loading, specifically when the "Resource Loading" and "ActorCreate" threads attempt to load modded actors. The root cause is a corrupted pointer in BotW's resource loading code that dereferences address `0xFFFFFFFFFFFEBC08` (~= signed `-0x143F8`). This happens 100% of the time, ~25-60 seconds after loading a save.

## The Crash

- **Faulting instruction**: `A9BD57F6` at U-King offset `0x12825C0` — this is an ARM64 `STP` (store pair) instruction
- **Faulting addresses**: `0xFFFFFFFFFFFEBC08`, `0xFFFFFFFFFFFEBC0C`, `0xFFFFFFFFFFFEBC10`, then `0x0000000000000000`
- **Pattern**: The sequence `BC08 → BC0C → BC08 → BC10 → 0000 → 0000` repeats identically on both "Resource Loading" (thread 57) and "ActorCreate" (thread 85) threads
- **Timing**: Always within the same millisecond — both threads hit the fault simultaneously
- **Stack trace** (consistent across all runs):
  ```
  U-King.nss:0x1d3f4a4
  U-King.nss:0x1d2800c
  U-King.nss:0x12823b4   ← resource/actor factory area
  U-King.nss:0x13c86a8
  U-King.nss:0x1c6d034
  U-King.nss:0x1c873f0
  ... (actor lifecycle chain)
  nnSdk:0x329e88
  nnSdk:0x32e294
  ```

## Most Likely Root Cause: RSTB (Resource Size Table) Mismatch

The address `0xFFFFFFFFFFFEBC08` is a sign-extended negative value (`-0x143F8`). This pattern is characteristic of **heap corruption caused by buffer overflow** — the game pre-allocates a resource buffer based on the RSTB entry, but the actual modded resource is larger than the RSTB says. The overflow corrupts adjacent heap metadata, and when the game later reads a pointer from that corrupted region, it gets `0xFFFFFFFFFFFEBC08` instead of a valid heap address.

### Evidence supporting RSTB theory:
1. The crash is in the resource loading path (stack traces show actor/resource factory code)
2. The corrupted value (`-0x143F8`) looks like corrupted heap metadata, not a code bug
3. Both "Resource Loading" and "ActorCreate" threads crash at the same offset — they're loading from the same corrupted heap
4. The Second Wind mod replaces **3753 files** including **2534 actor packs** — the RSTB must account for all of them
5. The mod's `System/Resource/ResourceSizeTable.product.srsizetable` is the RSTB override

### How the RSTB works in BotW:
- Before loading any resource, BotW checks the RSTB for the file's expected size
- It allocates a buffer of that size from a resource heap
- If the actual file is larger than the RSTB entry, the load overflows the buffer
- This corrupts adjacent allocations on the heap
- The corruption manifests later when the game accesses the corrupted region

## Investigation Steps for UKMM

### 1. Audit RSTB generation in `deploy.rs`

The RSTB update logic at `crates/uk-manager/src/deploy.rs:411-453` does:
```rust
if table.get(canon.as_str()).map(|s| s < size).unwrap_or(true) {
    table.set(canon.as_str(), size);
}
```

This only updates entries where the new size is **larger** than the existing entry. Potential issues:
- **Missing entries**: If a modded file has no RSTB entry at all, BotW may use a default allocation that's too small
- **Size calculation errors**: The `size` values come from `unpacker.unpack()` — if these are calculated incorrectly (e.g., not accounting for decompression overhead, alignment, or BotW's internal padding requirements), the game will overflow

### 2. Check RSTB size calculation

BotW's RSTB entries aren't just the file size — they include overhead for:
- Decompression buffers (files are often yaz0-compressed)
- Alignment padding (BotW aligns resource allocations)
- Internal parse overhead (SARC headers, AAMP parameter lists, etc.)
- A safety margin that Nintendo includes in stock entries

The RSTB size should be calculated as something like:
```
rstb_size = max(decompressed_size + parse_overhead + alignment, stock_rstb_entry)
```

If UKMM calculates raw file sizes without the overhead, entries will be too small.

### 3. Identify which specific resource triggers the crash

To narrow down which of the 2534 actor packs is causing the corruption, a binary search approach would work:
1. Deploy with only half the actor packs in the mod
2. If crash occurs → bad file is in that half
3. If no crash → bad file is in the other half
4. Repeat until the specific file(s) are found

Alternatively, add size validation: compare every modded file's actual decompressed size against its RSTB entry and flag any where `actual_size > rstb_entry`.

### 4. Check `ResourceSizeTable.product.srsizetable` directly

Dump the mod's RSTB and compare it against the stock RSTB:
- Are all 2534 actor pack files represented?
- Are the sizes adequate (>= decompressed size + overhead)?
- Are there any entries that were accidentally removed (`None` case in `apply_rstb`)?

## Files of Interest in UKMM

| File | Purpose |
|------|---------|
| `crates/uk-manager/src/deploy.rs:411-453` | RSTB generation and merging |
| `crates/uk-mod/src/unpack.rs` | Resource unpacking, produces RSTB size updates |
| `crates/uk-content/src/resource.rs` | Resource type definitions |
| `crates/uk-content/src/actor/` | Actor pack handling |

## Mod Structure (deployed)

```
SecondWind/romfs/
├── Actor/Pack/          (2534 .sbactorpack files)
├── Event/
├── Model/
├── Pack/                (Bootup.pack, Dungeon*.pack, etc.)
├── Physics/StaticCompound/MainField/
├── System/Resource/     (ResourceSizeTable.product.srsizetable)
└── UI/                  (PictureBook, StockItem)
```

## Ryujinx-Side Status

The Ryujinx emulator has been hardened to not crash on invalid memory accesses:
- Invalid reads/writes are caught and the guest thread is stopped gracefully
- Exception delivery follows the Switchbrew-documented Horizon kernel model
- Diagnostic logging captures fault address, PC, stack trace, and register dump

The remaining emulator crash is a JIT fast-path SIGSEGV when the masked garbage address falls outside the host memory backing — this is an emulator architectural limitation being tracked separately.

## Finding: ResidentActors.byml References Missing Actor

**Root cause of MainThread crash at ~21 seconds identified.**

The mod's `Actor/ResidentActors.byml` (inside Bootup.pack) adds 3 resident actors
not present in vanilla:
- `BluntArrow` — actor packs exist as `Obj_BluntArrow_A_01/02/03.sbactorpack`
- `Item_Feather` — actor packs exist as `Item_Feather_*.sbactorpack` (various suffixes)
- `SealingArrow` — **NO pack exists anywhere** (not in mod, not in vanilla)

When UKMM's `remerge` rebuilds Bootup.pack, it merges the mod's ResidentActors.byml
which includes `SealingArrow`. On game start, BotW tries to pre-load all resident
actors. It fails to find `SealingArrow`, gets a null resource handle, and the
MainThread crashes at U-King offset `0x1362A80`.

**Fix**: Use vanilla `ResidentActors.byml` in the deployed Bootup.pack. The hybrid
Bootup.pack builder (`bisect_bootup.py`) excludes file index 7 (ResidentActors.byml)
and uses all other 55 mod files.

**Broader concern**: This mod was designed for Wii U + BCML. There may be other
references to mod-only content that our Switch deployment doesn't handle. Need to
audit actor info tables, effect lists, and other cross-references for dangling
references.

## BFRES Converter Fix (Completed)

The BFRES converter had structural header bugs causing null pointer crashes in the
ActorCreate thread. These were fixed (see commit `0ff938f`):
- FVTX header was 20 bytes too short (76→96)
- FSHP header missing vertex_buffer_ptr field
- Mesh entries 12 bytes too short (44→56)
- Buffer data pointers were never linked

## RSTB Status

After the BFRES fix, `ukmm remerge && ukmm deploy` regenerates the RSTB with
correct sizes. The deployed RSTB must match the deployed files — manual file
replacement (bypassing UKMM) creates mismatches. Always use the UKMM pipeline,
then apply the Bootup.pack hybrid fix afterward.

## Quick Reproduction

```bash
# 1. Rebuild and deploy via UKMM
cd ~/fun/code/nintendo-second-wind-manager/UKMM
cargo build --release -p ukmm
./target/release/ukmm remerge
./target/release/ukmm deploy

# 2. Fix Bootup.pack (exclude mod's ResidentActors.byml)
cd ~/fun/code/nintendo-second-wind-manager
python3 bisect_bootup.py  # see indices
# Build hybrid with all mod files except index 7

# 3. Run
cd ~/fun/code/Ryujinx
./run.sh botw
```

Ryujinx logs are at `~/Library/Logs/Ryujinx/`
