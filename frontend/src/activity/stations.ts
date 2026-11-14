// The nine stations a call is sorted into: names, glyphs and the colour token each is drawn in.
// Glyph paths are the mockup's `KINDS` (notes/agent-activity-map/), drawn in a -10..10 box.
import type { ActivityStation } from '@/api/types'

export interface StationInfo {
  id: ActivityStation
  name: string
  /** Which hot colour the station's glyph takes (`--act-<tone>-hot`). */
  tone: 'live' | 'accent' | 'warn' | 'think'
  /** Where the dock sits on the map: -1 left (thinking and delegating), 1 right (hands-on work), 0 above (You). */
  side: -1 | 0 | 1
  /** The dock's row from the top on its side; You has none. */
  row: number
  /** An SVG path in a -10..10 box, stroked, never filled. */
  icon: string
}

const circle = (x: number, y: number, r: number) => `M${x - r} ${y}a${r} ${r} 0 1 0 ${2 * r} 0a${r} ${r} 0 1 0 ${-2 * r} 0`

export const STATIONS: readonly StationInfo[] = [
  { id: 'you', name: 'You', tone: 'warn', side: 0, row: -1,
    icon: `${circle(0, -4.6, 2.9)}M-6.4 7.4v-1.2a4.2 4.2 0 0 1 4.2 -4.2h4.4a4.2 4.2 0 0 1 4.2 4.2v1.2` },
  { id: 'model', name: 'Model', tone: 'think', side: -1, row: 0,
    icon: 'M0 -8C.9 -2.6 2.6 -.9 8 0C2.6 .9 .9 2.6 0 8C-.9 2.6 -2.6 .9 -8 0C-2.6 -.9 -.9 -2.6 0 -8z' },
  { id: 'agentflow', name: 'AgentFlow', tone: 'accent', side: -1, row: 1,
    icon: 'M-8 -3h5v6h-5zM3 -8h5v6h-5zM3 2h5v6h-5zM-3 0h3M0 -5v10M0 -5h3M0 5h3' },
  { id: 'files', name: 'Files', tone: 'live', side: 1, row: 0, icon: 'M-8 -5.5h5.2l2 2.3h8.8v9.2h-16z' },
  { id: 'shell', name: 'Shell', tone: 'live', side: 1, row: 1, icon: 'M-8 -6.5h16v13h-16zM-4.6 -2.2l3 2.4l-3 2.4M.6 3h4' },
  { id: 'git', name: 'Git', tone: 'live', side: 1, row: 2,
    icon: `${circle(-4.5, -5.5, 2.1)}${circle(-4.5, 5.5, 2.1)}${circle(5, -2.5, 2.1)}M-4.5 -3.4v6.8M5 -.4c0 3.6 -9.5 1.6 -9.5 4.6` },
  { id: 'network', name: 'Network', tone: 'live', side: 1, row: 3,
    icon: 'M-7.6 -1.6a10.8 10.8 0 0 1 15.2 0M-4.7 1.9a6.7 6.7 0 0 1 9.4 0M-1.9 5.2a2.7 2.7 0 0 1 3.8 0' },
  { id: 'agents', name: 'Agents', tone: 'live', side: -1, row: 3,
    icon: `M-8 0h4.5M-3.5 0c3.4 0 3 -5.2 6.6 -5.2M-3.5 0c3.4 0 3 5.2 6.6 5.2${circle(5.6, -5.2, 2.2)}${circle(5.6, 5.2, 2.2)}` },
  { id: 'skills', name: 'Skills', tone: 'live', side: -1, row: 2,
    icon: 'M0 -4.6c-2.2 -1.7 -4.8 -1.9 -7.6 -1.9v11c2.8 0 5.4 .2 7.6 1.9c2.2 -1.7 4.8 -1.9 7.6 -1.9v-11c-2.8 0 -5.4 .2 -7.6 1.9zM0 -4.6v11' },
]

const byId = new Map<string, StationInfo>(STATIONS.map((station) => [station.id, station]))

/** A station by id. An id this build does not know (a newer backend) is drawn as Shell. */
export function stationInfo(id: string): StationInfo {
  return byId.get(id) ?? byId.get('shell')!
}
