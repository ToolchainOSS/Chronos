/**
 * Application shell: a dual view layout with the logical topology panel on the
 * left and the geographic degradation panel on the right, plus a status header.
 */

import { useEffect } from "react";
import { GeoPanel } from "./GeoPanel";
import { LogicalPanel } from "./LogicalPanel";
import { useChronosStore } from "./store";
import { startDeltaClient } from "./ws";
import "./styles.css";

export function App(): JSX.Element {
  const status = useChronosStore((s) => s.status);
  const nodeCount = useChronosStore((s) => s.nodes.length);
  const linkCount = useChronosStore((s) => s.links.length);
  const deltasApplied = useChronosStore((s) => s.deltasApplied);

  // Open the delta stream once, on mount.
  useEffect(() => startDeltaClient(), []);

  return (
    <div className="app">
      <header className="app__header">
        <h1>Project Chronos</h1>
        <div className="app__stats">
          <span className={`status status--${status}`}>{status}</span>
          <span>ASNs: {nodeCount}</span>
          <span>Peerings: {linkCount}</span>
          <span>Deltas: {deltasApplied}</span>
        </div>
      </header>
      <main className="app__panels">
        <section className="panel panel--logical">
          <h2 className="panel__title">Logical Topology</h2>
          <div className="panel__body">
            <LogicalPanel />
          </div>
        </section>
        <section className="panel panel--geo">
          <h2 className="panel__title">Geographic Impact</h2>
          <div className="panel__body">
            <GeoPanel />
          </div>
        </section>
      </main>
    </div>
  );
}
