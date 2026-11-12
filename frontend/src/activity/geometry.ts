// The hull-and-docks map's geometry, in viewBox units: where each dock sits, the cable that joins it
// to the hull, the trail between docks and a subagent's hops. Pure functions of the map's width, so
// they can be run with node (see `check.mts`). Values are the mockup's `mapGeom`, with the
// compacted rows `build-v3.js` applies (spec §4).
import type { ActivityEntry, ActivityStation } from '@/api/types'
import { STATIONS } from './stations.ts'

/** The map's height; its width is the panel's. */
export const MAP_H = 300
export const MAP_MIN_W = 250
export const MAP_MAX_W = 560
/** Dock rows, top to bottom, on both sides. */
export const ROWS = [90, 152, 214, 276] as const
/** The trail joins this many of the agent's last distinct dock visits. */
export const TRAIL = 6

export interface Pt { x: number; y: number }

export interface DockGeom {
  id: ActivityStation
  side: -1 | 0 | 1
  row: number
  /** The plate's left edge and vertical centre. */
  x: number
  y: number
  /** The plate's width (the You plate is 150). */
  w: number
  /** The squircle's centre, where the glyph sits. */
  sx: number
  sy: number
  /** Where the cable plugs into the dock. */
  port: Pt
  /** Where the cable leaves the hull. */
  hub: Pt
}

export interface MapGeom {
  W: number
  small: boolean
  dw: number
  pad: number
  /** The hull's full width, and half of it. */
  hull: number
  half: number
  /** How far the hubs sit from the hull's centre. */
  reach: number
  cx: number
  cy: number
  at: Record<ActivityStation, DockGeom>
  /** The cable from each dock's hub to its port, as an SVG path. */
  cable: Record<ActivityStation, string>
  /** Where a subagent's pod waits under the hull. */
  bay: Pt
  /** The gap between a dock plate and the hull, used to bow trail legs. */
  gap: number
}

export const r1 = (value: number) => Math.round(value * 10) / 10

const clamp = (value: number, low: number, high: number) => Math.max(low, Math.min(high, value))

const point = (p: Pt) => `${r1(p.x)} ${r1(p.y)}`

/** A cubic Bézier path from `p0` to `p3`. */
export function cubic(p0: Pt, c1: Pt, c2: Pt, p3: Pt): string {
  return `M${point(p0)}C${point(c1)} ${point(c2)} ${point(p3)}`
}

/** The width the map is drawn at for a panel this wide. */
export const mapWidth = (panelWidth: number) => Math.min(MAP_MAX_W, Math.max(MAP_MIN_W, Math.round(panelWidth)))

const cache = new Map<number, MapGeom>()

/** The map's geometry at width `W` (cached; the values never change for one width). */
export function mapGeom(W: number): MapGeom {
  const known = cache.get(W)
  if (known) return known
  const small = W < 400
  const dw = small ? 96 : clamp(Math.round(W * 0.235), 104, 124)
  const pad = small ? 8 : 12
  const hull = small ? 76 : 92
  const cx = W / 2
  const cy = 183
  const half = hull / 2
  const reach = half + 9
  const at = {} as Record<ActivityStation, DockGeom>
  const cable = {} as Record<ActivityStation, string>
  for (const station of STATIONS) {
    const { id, side, row } = station
    if (side === 0) {
      at[id] = { id, side, row, x: cx - 75, y: 28, w: 150, sx: cx, sy: 28, port: { x: cx, y: 43 }, hub: { x: cx, y: cy - reach } }
      cable[id] = `M${point(at[id].hub)}L${point(at[id].port)}`
      continue
    }
    const y = ROWS[row]
    const hub = { x: cx + side * reach, y: cy + (row - 1.5) * 18 }
    const x = side < 0 ? pad : W - pad - dw
    const sx = side < 0 ? x + dw - 16 : x + 16
    const port = { x: sx + side * -15, y }
    at[id] = { id, side, row, x, y, w: dw, sx, sy: y, port, hub }
    const dx = (port.x - hub.x) * 0.55
    cable[id] = cubic(hub, { x: hub.x + dx, y: hub.y }, { x: port.x - dx, y: port.y }, port)
  }
  const geom: MapGeom = { W, small, dw, pad, hull, half, reach, cx, cy, at, cable, bay: { x: cx, y: 268 }, gap: cx - reach - (pad + dw) }
  cache.set(W, geom)
  return geom
}

/**
 * One leg of the trail, from one dock's port to another's. Legs on the same side bow toward the
 * hull; legs that cross sides pass behind it.
 */
export function hopLeg(g: MapGeom, a: ActivityStation, b: ActivityStation): string {
  const A = g.at[a]
  const B = g.at[b]
  const same = A.side !== 0 && A.side === B.side
  const k = same ? Math.min(14 + 9 * Math.abs(A.row - B.row), g.gap * 0.95) : 58
  const pull = (dock: DockGeom): Pt =>
    dock.side === 0 ? { x: dock.port.x, y: dock.port.y + 44 } : { x: dock.port.x - dock.side * k, y: dock.port.y }
  return cubic(A.port, pull(A), pull(B), B.port)
}

/** The dock visits the trail draws: the agent's own calls, model steps skipped, repeats merged. */
export interface Visit { id: ActivityStation; at: number }
export function visitsOf(entries: Pick<ActivityEntry, 'station' | 'bySubagent' | 'startedAt'>[]): Visit[] {
  const out: Visit[] = []
  for (const entry of entries) {
    if (entry.bySubagent || entry.station === 'model') continue
    const at = Date.parse(entry.startedAt)
    const tail = out[out.length - 1]
    if (tail && tail.id === entry.station) tail.at = at
    else out.push({ id: entry.station, at })
  }
  return out.slice(-TRAIL)
}

/** A subagent's hop: from its pod in the bay out to the dock it is using. */
export function podHop(g: MapGeom, pod: Pt, station: ActivityStation): string {
  const A = g.at[station]
  const c1 = A.side === 0 ? { x: pod.x, y: pod.y - 30 } : { x: pod.x + A.side * 44, y: pod.y }
  const c2 = A.side === 0 ? { x: A.port.x, y: A.port.y + 40 } : { x: A.port.x - A.side * 40, y: A.port.y }
  return cubic(pod, c1, c2, A.port)
}

/** The bay slot of the `k`th of `n` pods. */
export function podSlot(g: MapGeom, k: number, n: number): Pt {
  return { x: g.bay.x + (k - (n - 1) / 2) * 38, y: g.bay.y }
}

/** A rounded square of half-size `h`, drawn from the top middle so a dash gauge starts there. */
export function hullPath(cx: number, cy: number, h: number, r: number): string {
  return `M${cx} ${cy - h}H${cx + h - r}A${r} ${r} 0 0 1 ${cx + h} ${cy - h + r}V${cy + h - r}A${r} ${r} 0 0 1 ${cx + h - r} ${cy + h}` +
    `H${cx - h + r}A${r} ${r} 0 0 1 ${cx - h} ${cy + h - r}V${cy - h + r}A${r} ${r} 0 0 1 ${cx - h + r} ${cy - h}z`
}
