/**
 * Delta frame types mirroring the Rust `chronos_types::Delta` enum.
 *
 * The wire form is an internally tagged JSON object keyed by `kind`.
 */

export interface LinkUp {
  kind: "LinkUp";
  a: number;
  b: number;
}

export interface LinkDown {
  kind: "LinkDown";
  a: number;
  b: number;
}

export interface AreaDegraded {
  kind: "AreaDegraded";
  region: string;
  severity: number;
}

export type Delta = LinkUp | LinkDown | AreaDegraded;

/** A logical graph node (an ASN). */
export interface GraphNode {
  id: number;
}

/** A logical graph link (a peering between two ASNs). */
export interface GraphLink {
  source: number;
  target: number;
}
