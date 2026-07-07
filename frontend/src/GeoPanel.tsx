/**
 * Geographic boundary panel (blueprint Task 5.3).
 *
 * Renders administrative boundaries with Maplibre GL (via react-map-gl). When an
 * `AreaDegraded` delta arrives, the affected polygon's `severity` feature state is
 * updated so its fill opacity changes; using feature state (rather than swapping
 * the source data) avoids a full map re-render on every update.
 *
 * The boundary vector set and base style are open source and configured by URL:
 * - VITE_MAP_STYLE: a Maplibre style JSON URL (defaults to a demo style).
 * - VITE_BOUNDARIES_URL: a GeoJSON of admin boundaries whose features carry an
 *   ISO code property (`iso_3166_2` for subdivisions, falling back to a country
 *   code) matching the region codes emitted by the backend.
 */

import { useCallback, useEffect, useMemo, useRef } from "react";
import Map, {
  Layer,
  Source,
  type FillLayer,
  type MapRef,
} from "react-map-gl/maplibre";
import "maplibre-gl/dist/maplibre-gl.css";
import { useChronosStore } from "./store";

const MAP_STYLE =
  import.meta.env.VITE_MAP_STYLE ??
  "https://demotiles.maplibre.org/style.json";

const BOUNDARIES_URL =
  import.meta.env.VITE_BOUNDARIES_URL ??
  "https://demotiles.maplibre.org/style.json";

const SOURCE_ID = "admin-boundaries";

const degradationLayer: FillLayer = {
  id: "degradation-fill",
  type: "fill",
  source: SOURCE_ID,
  paint: {
    "fill-color": "#ff4d4d",
    // Opacity is driven by the per feature `severity` state (0 when unset).
    "fill-opacity": [
      "coalesce",
      ["feature-state", "severity"],
      0,
    ],
  },
};

export function GeoPanel(): JSX.Element {
  const mapRef = useRef<MapRef | null>(null);
  const regionSeverity = useChronosStore((s) => s.regionSeverity);
  // Track which region ids we have applied so we can update only the deltas.
  const applied = useRef<Record<string, number>>({});

  const applyState = useCallback(() => {
    const map = mapRef.current?.getMap();
    if (!map || !map.getSource(SOURCE_ID)) {
      return;
    }
    for (const [region, severity] of Object.entries(regionSeverity)) {
      if (applied.current[region] === severity) {
        continue;
      }
      map.setFeatureState(
        { source: SOURCE_ID, id: region },
        { severity },
      );
      applied.current[region] = severity;
    }
  }, [regionSeverity]);

  useEffect(() => {
    applyState();
  }, [applyState]);

  const initialViewState = useMemo(
    () => ({ longitude: 10, latitude: 30, zoom: 1.4 }),
    [],
  );

  return (
    <Map
      ref={mapRef}
      mapStyle={MAP_STYLE}
      initialViewState={initialViewState}
      onLoad={applyState}
      style={{ width: "100%", height: "100%" }}
    >
      <Source
        id={SOURCE_ID}
        type="geojson"
        data={BOUNDARIES_URL}
        // Promote the ISO code property to the feature id so setFeatureState can
        // target a region by its code.
        promoteId="iso_3166_2"
      >
        <Layer {...degradationLayer} />
      </Source>
    </Map>
  );
}
