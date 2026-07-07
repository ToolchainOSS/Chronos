/**
 * Global client state (blueprint Task 5.1).
 *
 * A Zustand store keeps two decoupled data structures:
 * - a logical graph (nodes plus links) consumed by the force graph panel; and
 * - a geographic impact map (region code to severity) consumed by the map panel.
 *
 * Zustand is used instead of React Context so that high frequency delta updates
 * do not force the entire component tree to re-render: components subscribe only
 * to the slices they read.
 */

import { create } from "zustand";
import type { Delta, GraphLink, GraphNode } from "./types";

/** Connection lifecycle status surfaced in the UI. */
export type ConnectionStatus = "connecting" | "open" | "closed";

/** A link removal event, surfaced so the panel can animate before pruning. */
export interface PendingRemoval {
  source: number;
  target: number;
  at: number;
}

interface ChronosState {
  status: ConnectionStatus;
  nodes: GraphNode[];
  links: GraphLink[];
  regionSeverity: Record<string, number>;
  /** Links flagged for the dissipation animation before removal. */
  removing: PendingRemoval[];
  deltasApplied: number;

  setStatus: (status: ConnectionStatus) => void;
  applyDelta: (delta: Delta) => void;
  clearRemoval: (source: number, target: number) => void;
}

/** Canonical key for an undirected link so (a, b) and (b, a) match. */
function linkKey(a: number, b: number): string {
  return a <= b ? `${a}-${b}` : `${b}-${a}`;
}

export const useChronosStore = create<ChronosState>((set) => ({
  status: "connecting",
  nodes: [],
  links: [],
  regionSeverity: {},
  removing: [],
  deltasApplied: 0,

  setStatus: (status) => set({ status }),

  clearRemoval: (source, target) =>
    set((state) => ({
      removing: state.removing.filter(
        (r) => linkKey(r.source, r.target) !== linkKey(source, target),
      ),
    })),

  applyDelta: (delta) =>
    set((state) => {
      const next = { deltasApplied: state.deltasApplied + 1 };

      switch (delta.kind) {
        case "LinkUp": {
          const key = linkKey(delta.a, delta.b);
          const exists = state.links.some(
            (l) => linkKey(l.source, l.target) === key,
          );
          if (exists) {
            return next;
          }
          const nodeIds = new Set(state.nodes.map((n) => n.id));
          const newNodes = [...state.nodes];
          if (!nodeIds.has(delta.a)) {
            newNodes.push({ id: delta.a });
          }
          if (!nodeIds.has(delta.b)) {
            newNodes.push({ id: delta.b });
          }
          return {
            ...next,
            nodes: newNodes,
            links: [...state.links, { source: delta.a, target: delta.b }],
          };
        }

        case "LinkDown": {
          const key = linkKey(delta.a, delta.b);
          const exists = state.links.some(
            (l) => linkKey(l.source, l.target) === key,
          );
          if (!exists) {
            return next;
          }
          // Flag for animation; the panel prunes it after the effect completes.
          const already = state.removing.some(
            (r) => linkKey(r.source, r.target) === key,
          );
          return {
            ...next,
            removing: already
              ? state.removing
              : [
                  ...state.removing,
                  { source: delta.a, target: delta.b, at: Date.now() },
                ],
          };
        }

        case "AreaDegraded": {
          return {
            ...next,
            regionSeverity: {
              ...state.regionSeverity,
              [delta.region]: delta.severity,
            },
          };
        }

        default:
          return next;
      }
    }),
}));

/**
 * Remove a link from the logical graph. Called by the panel once the
 * dissipation animation for a `LinkDown` has finished.
 */
export function pruneLink(source: number, target: number): void {
  useChronosStore.setState((state) => {
    const key = linkKey(source, target);
    const links = state.links.filter(
      (l) => linkKey(l.source, l.target) !== key,
    );
    // Drop nodes that no longer participate in any link.
    const referenced = new Set<number>();
    for (const l of links) {
      referenced.add(l.source);
      referenced.add(l.target);
    }
    const nodes = state.nodes.filter((n) => referenced.has(n.id));
    return {
      links,
      nodes,
      removing: state.removing.filter(
        (r) => linkKey(r.source, r.target) !== key,
      ),
    };
  });
}
