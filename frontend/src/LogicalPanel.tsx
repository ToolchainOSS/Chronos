/**
 * Logical topology panel (blueprint Task 5.2).
 *
 * Renders ASNs as nodes and peerings as links using `react-force-graph-2d`
 * (a WebGL/canvas force directed graph). When a `LinkDown` delta arrives the
 * store flags the link for removal; this panel paints a brief particle
 * dissipation effect along that link, then prunes it from the graph.
 */

import { useEffect, useRef } from "react";
import ForceGraph2D, { type ForceGraphMethods } from "react-force-graph-2d";
import { pruneLink, useChronosStore } from "./store";
import type { GraphLink, GraphNode } from "./types";

const REMOVAL_ANIMATION_MS = 1200;

function linkKey(a: number, b: number): string {
  return a <= b ? `${a}-${b}` : `${b}-${a}`;
}

export function LogicalPanel(): JSX.Element {
  const nodes = useChronosStore((s) => s.nodes);
  const links = useChronosStore((s) => s.links);
  const removing = useChronosStore((s) => s.removing);
  const graphRef =
    useRef<ForceGraphMethods<GraphNode, GraphLink> | undefined>(undefined);

  // Drive particle emitters on links flagged for removal, then prune them once
  // the dissipation animation has elapsed.
  useEffect(() => {
    if (removing.length === 0) {
      return;
    }
    const timers = removing.map((r) =>
      window.setTimeout(() => pruneLink(r.source, r.target), REMOVAL_ANIMATION_MS),
    );
    return () => {
      for (const t of timers) {
        window.clearTimeout(t);
      }
    };
  }, [removing]);

  const removingKeys = new Set(
    removing.map((r) => linkKey(r.source, r.target)),
  );

  return (
    <ForceGraph2D<GraphNode, GraphLink>
      ref={graphRef}
      graphData={{ nodes, links }}
      nodeId="id"
      nodeLabel={(n) => `AS${(n as GraphNode).id}`}
      nodeRelSize={4}
      nodeColor={() => "#6ee7ff"}
      linkColor={(link) => {
        const l = link as GraphLink;
        return removingKeys.has(linkKey(l.source, l.target))
          ? "#ff4d4d"
          : "rgba(160, 190, 210, 0.35)";
      }}
      linkWidth={(link) => {
        const l = link as GraphLink;
        return removingKeys.has(linkKey(l.source, l.target)) ? 2.5 : 0.6;
      }}
      // Emit particles along links that are dissipating (the LinkDown effect).
      linkDirectionalParticles={(link) => {
        const l = link as GraphLink;
        return removingKeys.has(linkKey(l.source, l.target)) ? 8 : 0;
      }}
      linkDirectionalParticleWidth={3}
      linkDirectionalParticleColor={() => "#ff8080"}
      backgroundColor="#0b1020"
      cooldownTicks={80}
    />
  );
}
