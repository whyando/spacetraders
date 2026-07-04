#!/usr/bin/env python3
"""Animate gate-network exploration expansion from an events bundle.

Renders selected strategies side-by-side over a shared wall-clock, showing the
charted frontier grow across the galaxy. Output: an animated GIF.

Pipeline (Scenario 1 visualization):
  1. cargo run --release --bin explore_sim -- --mode t5+capitals --emit events.json
  2. python3 tools/animate_expansion.py \
         --bundle events.json \
         --universe tests/fixtures/galaxy_2026-07-04/universe.json \
         --out expansion.gif --strategies baseline beam

Requires: matplotlib, numpy, pillow (e.g. a venv). imagemagick `convert
expansion.gif -layers optimize -fuzz 3% +map out.gif` shrinks the result.
"""
import argparse
import json
import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.collections import LineCollection
from matplotlib.animation import FuncAnimation, PillowWriter
from matplotlib.lines import Line2D

# --- palette (dark "space" surface; cividis = CVD-safe sequential for time) ---
BG = "#080b12"
PANEL = "#0b0f18"
DOT_UNCHARTED = "#243049"
EDGE = "#33415c"
INK = "#e6ebf5"
MUTED = "#8792a8"
TARGET_PENDING = "#8792a8"
TARGET_REACHED = "#ffb703"
HOME = "#ff5d8f"
# truncate cividis to [0.25,1] so the earliest-charted points still read on dark
CMAP = matplotlib.colors.LinearSegmentedColormap.from_list(
    "cividis_hi", plt.get_cmap("cividis")(np.linspace(0.25, 1.0, 256)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bundle", required=True)
    ap.add_argument("--universe", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--strategies", nargs="+", default=None,
                    help="substrings of strategy names to show (default: baseline + beam)")
    ap.add_argument("--frames", type=int, default=170)
    ap.add_argument("--fps", type=int, default=18)
    args = ap.parse_args()

    bundle = json.load(open(args.bundle))
    uni = json.load(open(args.universe))

    # gate-system coordinates
    coords = {}
    for s in uni["systems"]:
        if s.get("has_gate"):
            coords[s["symbol"]] = (s["x"], s["y"])
    syms = list(coords.keys())
    idx = {s: i for i, s in enumerate(syms)}
    XY = np.array([coords[s] for s in syms], dtype=float)

    # edges among gate systems (both endpoints must be known gate systems)
    E = [(idx[a], idx[b]) for a, b in uni["edges"] if a in idx and b in idx]
    E = np.array(E, dtype=int) if E else np.zeros((0, 2), int)

    home = bundle["home"]
    targets = [t for t in bundle["targets"] if t in idx]
    tgt_idx = np.array([idx[t] for t in targets], dtype=int)

    # choose strategies to show
    picks = args.strategies or ["baseline", "beam"]
    strategies = []
    for p in picks:
        m = next((s for s in bundle["strategies"] if p.lower() in s["name"].lower()), None)
        if m:
            strategies.append(m)
    assert strategies, "no strategies matched"

    T_END = max(s["makespan"] for s in bundle["strategies"])  # shared time scale
    norm = matplotlib.colors.Normalize(vmin=0, vmax=T_END)

    # per-strategy: charting time per gate-system index (inf if never charted)
    packs = []
    for s in strategies:
        ct = np.full(len(syms), np.inf)
        for t, sym, _is_t in s["events"]:
            if sym in idx:
                ct[idx[sym]] = t
        ct[idx[home]] = 0.0  # home charted at t=0
        edge_appear = np.maximum(ct[E[:, 0]], ct[E[:, 1]]) if len(E) else np.zeros(0)
        packs.append(dict(meta=s, ct=ct, edge_appear=edge_appear))

    # --- figure ---
    n = len(strategies)
    fig, axes = plt.subplots(1, n, figsize=(7.4 * n, 7.8), facecolor=BG)
    if n == 1:
        axes = [axes]
    pad = 0.03 * np.ptp(XY[:, 0])
    xlim = (XY[:, 0].min() - pad, XY[:, 0].max() + pad)
    ylim = (XY[:, 1].min() - pad, XY[:, 1].max() + pad)

    artists = []
    for ax, pk in zip(axes, packs):
        ax.set_facecolor(PANEL)
        ax.set_xlim(*xlim); ax.set_ylim(*ylim)
        ax.set_aspect("equal"); ax.set_xticks([]); ax.set_yticks([])
        for sp in ax.spines.values():
            sp.set_color("#1c2740")
        # static context: all gate systems faint
        ax.scatter(XY[:, 0], XY[:, 1], s=1.2, c=DOT_UNCHARTED, linewidths=0, zorder=1)
        edges_lc = LineCollection([], colors=EDGE, linewidths=0.35, alpha=0.5, zorder=2)
        ax.add_collection(edges_lc)
        charted = ax.scatter([], [], s=7, c=[], cmap=CMAP, norm=norm,
                             linewidths=0, zorder=3)
        pending = ax.scatter(XY[tgt_idx, 0], XY[tgt_idx, 1], s=70, marker="*",
                             facecolors="none", edgecolors=TARGET_PENDING,
                             linewidths=0.8, zorder=4)
        reached = ax.scatter([], [], s=90, marker="*", facecolors=TARGET_REACHED,
                             edgecolors="#3a2a00", linewidths=0.4, zorder=5)
        ax.scatter([XY[idx[home], 0]], [XY[idx[home], 1]], s=150, marker="o",
                   facecolors="none", edgecolors=HOME, linewidths=1.8, zorder=6)
        title = ax.set_title("", color=INK, fontsize=13, pad=10, loc="left")
        artists.append(dict(edges=edges_lc, charted=charted, reached=reached,
                            title=title))

    fig.patch.set_facecolor(BG)
    fig.suptitle(f"Gate-network exploration from {home} — 20 probes",
                 color=INK, fontsize=16, y=0.985, x=0.5)
    # legend (identity by shape+fill, not color alone)
    leg = [
        Line2D([0], [0], marker="o", color="none", markerfacecolor="none",
               markeredgecolor=HOME, markeredgewidth=1.8, markersize=11, label="home"),
        Line2D([0], [0], marker="*", color="none", markerfacecolor="none",
               markeredgecolor=TARGET_PENDING, markersize=13, label="target (pending)"),
        Line2D([0], [0], marker="*", color="none", markerfacecolor=TARGET_REACHED,
               markeredgecolor="#3a2a00", markersize=13, label="target (reached)"),
        Line2D([0], [0], marker="o", color="none", markerfacecolor=CMAP(0.5),
               markersize=9, label="charted (shade = time)"),
    ]
    fig.legend(handles=leg, loc="lower center", ncol=4, frameon=False,
               labelcolor=MUTED, fontsize=11, bbox_to_anchor=(0.5, 0.005))
    # colorbar for time
    sm = plt.cm.ScalarMappable(cmap=CMAP, norm=norm)
    cb = fig.colorbar(sm, ax=axes, fraction=0.025, pad=0.01)
    cb.set_label("charted at (hours)", color=MUTED, fontsize=10)
    cb.ax.yaxis.set_tick_params(color=MUTED)
    cb.set_ticks(np.linspace(0, T_END, 5))
    cb.set_ticklabels([f"{v/3600:.0f}" for v in np.linspace(0, T_END, 5)])
    plt.setp(cb.ax.get_yticklabels(), color=MUTED)
    cb.outline.set_edgecolor("#1c2740")

    fig.subplots_adjust(left=0.01, right=0.93, top=0.85, bottom=0.09, wspace=0.03)

    times = np.linspace(0, T_END * 1.02, args.frames)
    HOLD = 14  # hold on the final frame

    def update(fi):
        tk = times[min(fi, len(times) - 1)]
        out = []
        for a, pk in zip(artists, packs):
            ct = pk["ct"]
            mask = ct <= tk
            a["charted"].set_offsets(XY[mask])
            a["charted"].set_array(ct[mask])
            if len(E):
                em = pk["edge_appear"] <= tk
                a["edges"].set_segments([[XY[i], XY[j]] for i, j in E[em]])
            rmask = ct[tgt_idx] <= tk
            a["reached"].set_offsets(XY[tgt_idx[rmask]] if rmask.any() else np.empty((0, 2)))
            nchart = int(mask.sum())
            nreach = int(rmask.sum())
            a["title"].set_text(
                f"{pk['meta']['name']}\n"
                f"t = {tk/3600:5.1f} h   •   charted {nchart}   •   "
                f"reached {nreach}/{len(tgt_idx)}")
            out += [a["charted"], a["edges"], a["reached"], a["title"]]
        return out

    total = args.frames + HOLD
    anim = FuncAnimation(fig, update, frames=total, interval=1000 / args.fps, blit=False)
    anim.save(args.out, writer=PillowWriter(fps=args.fps),
              savefig_kwargs={"facecolor": BG})
    print("wrote", args.out)


if __name__ == "__main__":
    main()
