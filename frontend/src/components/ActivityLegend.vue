<script setup lang="ts">
// "How to read it": the Details drawer's seventh tab. The text and the little pictures are the
// approved mockup's legend (notes/agent-activity-map/v3-taskpage/part-legend.html and
// v3-legend-canvas.html). The pictures read the app's tokens, so they follow the theme.
</script>

<template>
  <div class="legend">
    <div class="why">
      <p><b>Why this shape.</b> The agent is the hull in the middle, and every call is a packet that visibly leaves it, reaches a dock and comes back with the result. The docks are placed by what they mean, so position alone tells you something: you sit above the agent, thinking and delegating are on the left, hands-on work on the files, shell, git and network is on the right.</p>
      <p>The map answers "what is it touching right now". The tape, one click away, answers "when, and for how long". Run so far keeps the whole run on screen under both.</p>
    </div>
    <h3>On the map</h3>
    <dl>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><path d="M2 19C18 19 24 5 42 5" fill="none" stroke-width="1.4" style="stroke:var(--border-strong)"/><path d="M14 16.2C19 13.6 22 10.6 27 8.2" fill="none" stroke-width="5" stroke-linecap="round" style="stroke:var(--live-hot)"/></svg></dt>
      <dd>A packet leaves the hull when a call starts and comes back when it ends. <span>Grey on the way back for a result, red for a failure. A small ripple marks where the result lands. A hollow packet is something arriving from you.</span></dd>
      <dt><svg width="44" height="26" viewBox="0 0 44 26"><rect x="10" y="3" width="20" height="20" rx="7" stroke-width="1.2" style="fill:var(--panel);stroke:var(--border-strong)"/><rect x="7" y="0.5" width="26" height="25" rx="9" fill="none" stroke-width="2.2" pathLength="1" stroke-dasharray=".62 1" stroke-linecap="round" style="stroke:var(--live-hot)"/><text x="36" y="16" font-family="JetBrains Mono, monospace" font-size="7" style="fill:var(--text)">6s</text></svg></dt>
      <dd>While a call runs, current flows along its cable, the dock shows a timer, and a border runs round the dock's glyph once every 10 seconds.</dd>
      <dt><svg width="44" height="20" viewBox="0 0 44 20"><path d="M2 6h40" stroke-width="1.2" stroke-dasharray="1.5 4.5" stroke-linecap="round" style="stroke:var(--border-strong)"/><path d="M2 14h40" stroke-width="4" stroke-linecap="round" opacity=".6" style="stroke:var(--live)"/></svg></dt>
      <dd>A cable thickens each time it is used. <span>A dotted cable has not been used at all, so by the end the map shows where the work went.</span></dd>
      <dt><svg width="44" height="22" viewBox="0 0 44 22"><path d="M3 4C16 4 16 18 3 18" fill="none" stroke-width="1.2" opacity=".45" style="stroke:var(--text)"/><path d="M3 18C22 18 22 8 40 8" fill="none" stroke-width="1.5" opacity=".8" style="stroke:var(--text)"/><circle cx="40" cy="8" r="2.3" style="fill:var(--text)"/></svg></dt>
      <dd>A thin line joins the last few docks the agent visited, in order. <span>The newest leg draws itself; older legs fade.</span></dd>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><circle cx="12" cy="12" r="8" style="fill:var(--raised);stroke:var(--live-hot)"/><text x="12" y="15.2" text-anchor="middle" font-family="JetBrains Mono, monospace" font-size="8.5" style="fill:var(--text)">×3</text><circle cx="33" cy="12" r="7.5" style="fill:var(--danger)"/><text x="33" y="15.4" text-anchor="middle" font-family="Inter, sans-serif" font-size="9.5" font-weight="700" style="fill:var(--panel)">1</text></svg></dt>
      <dd>×3 means three calls are out at that dock together; their packets fly side by side. <span>A red count stays on a dock after a call there fails. The dock shakes when it happens.</span></dd>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><line x1="2" y1="12" x2="14" y2="12" stroke-dasharray="1.5 3.5" style="stroke:var(--live)"/><rect x="14" y="3" width="18" height="18" rx="6" stroke-width="1.5" style="fill:var(--raised);stroke:var(--live-hot)"/><path d="M32 12C38 12 38 4 43 4" fill="none" stroke-dasharray="2 4" style="stroke:var(--live)"/></svg></dt>
      <dd>A subagent undocks from Agents as a pod and waits under the hull. <span>Its own calls leave from the pod along dashed lines, so you can tell them from the agent's.</span></dd>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><rect x="1" y="1" width="42" height="22" rx="10" fill="none" opacity=".45" style="stroke:var(--warn-hot)"/><rect x="6" y="5" width="32" height="14" rx="7" style="fill:var(--warn-soft);stroke:var(--warn)"/><text x="22" y="15.4" text-anchor="middle" font-family="Inter, sans-serif" font-size="8" font-weight="600" style="fill:var(--text)">You</text></svg></dt>
      <dd>When the agent needs your answer, the You dock turns amber and sends out slow rings, and everything else dims. <span>The canvas uses the same amber for the same thing.</span></dd>
      <dt><svg width="44" height="26" viewBox="0 0 44 26"><rect x="11" y="2" width="22" height="22" rx="8" fill="none" stroke-width="2.4" style="stroke:var(--border)"/><rect x="11" y="2" width="22" height="22" rx="8" fill="none" stroke-width="2.4" pathLength="100" stroke-dasharray="58 100" stroke-linecap="round" style="stroke:var(--muted)"/></svg></dt>
      <dd>The border round the hull fills as the context window is used. <span>Amber from 70%, red from 90%.</span></dd>
    </dl>
    <h3>On the tape and the strip</h3>
    <dl>
      <dt><svg width="44" height="20" viewBox="0 0 44 20"><rect x="2" y="5" width="14" height="10" rx="3" opacity=".55" style="fill:var(--live)"/><rect x="20" y="5" width="16" height="10" rx="3" style="fill:var(--live-hot)"/><circle cx="38" cy="10" r="5" stroke-width="1.5" style="fill:var(--raised);stroke:var(--live)"/></svg></dt>
      <dd>On the tape a bar is one call and its length is how long it took. <span>Each kind has a lane. Parallel calls split the lane, a subagent's calls are hollow, a failure is red, and an amber band is time spent waiting for you. Hover a bar to name the call.</span></dd>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><rect x="2" y="3" width="3" height="3" style="fill:var(--live)"/><rect x="8" y="8" width="7" height="3" style="fill:var(--live)"/><rect x="18" y="3" width="2" height="3" style="fill:var(--accent)"/><rect x="23" y="8" width="9" height="3" style="fill:var(--live)"/><path d="M2 21h10v-2h12v-3h18" fill="none" stroke-width="1.4" style="stroke:var(--muted)"/></svg></dt>
      <dd>Run so far squeezes the whole run into one strip, one row per kind. <span>The line beneath is the context window filling up.</span></dd>
    </dl>
    <h4>On the canvas</h4>
    <dl>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><line x1="2" y1="18" x2="42" y2="18" stroke-width="1.5" style="stroke:var(--border-strong)"/><line x1="12" y1="18" x2="26" y2="18" stroke-width="4" stroke-linecap="round" style="stroke:var(--accent)"/><rect x="7" y="1" width="32" height="12" rx="6" style="fill:var(--raised);stroke:var(--accent)"/><text x="23" y="9.6" text-anchor="middle" font-family="JetBrains Mono, monospace" font-size="7" style="fill:var(--text)">4f2a91c</text></svg></dt>
      <dd>A baton carries what is handed over along the edge: requirements, a checkpoint, the blocking findings. <span>Amber when Consolidate Findings sends work back for fixes.</span></dd>
      <dt><svg width="44" height="20" viewBox="0 0 44 20"><rect x="2" y="2" width="40" height="3.6" rx="1.8" style="fill:var(--border)"/><rect x="2" y="8.2" width="40" height="3.6" rx="1.8" style="fill:var(--border)"/><rect x="2" y="14.4" width="40" height="3.6" rx="1.8" style="fill:var(--border)"/><rect x="5" y="2" width="5" height="3.6" style="fill:var(--live)"/><rect x="14" y="2" width="14" height="3.6" style="fill:var(--live)"/><rect x="6" y="8.2" width="9" height="3.6" style="fill:var(--live)"/><rect x="20" y="8.2" width="3" height="3.6" style="fill:var(--accent)"/><rect x="4" y="14.4" width="3" height="3.6" style="fill:var(--accent)"/><rect x="12" y="14.4" width="18" height="3.6" style="fill:var(--live)"/></svg></dt>
      <dd>Each agent card shows the call it is on now and a small strip of its calls, one row per worker. <span>The strip is the same picture as Run so far.</span></dd>
      <dt><svg width="44" height="24" viewBox="0 0 44 24"><rect x="3" y="3" width="38" height="18" rx="7" fill="none" stroke-width="2.6" style="stroke:var(--warn)"/></svg></dt>
      <dd>A card waiting for you has an amber border, whether it needs an answer or an approval. <span>Today a question shows red; red is now kept for failures and blocking findings.</span></dd>
      <dt><svg width="44" height="18" viewBox="0 0 44 18"><rect x="1" y="2" width="42" height="14" rx="4" style="fill:var(--raised)"/><text x="5" y="12.4" font-family="JetBrains Mono, monospace" font-size="7" style="fill:var(--text)">0:12</text><text x="40" y="12.4" text-anchor="end" font-family="JetBrains Mono, monospace" font-size="7" style="fill:var(--text)">~NZ$0.65</text></svg></dt>
      <dd>The card footer shows cost and tokens as they accrue. <span>Today the cost appears only after a run ends.</span></dd>
    </dl>
    <p class="muted small" style="margin-top: 1rem">The canvas now follows the app theme: paper and white cards in light, today's black cards in dark. The camera keeps the active node, or the group running together, in the middle, and stops at the first and last nodes so no empty space shows beyond them. The rest of the page is the task page as it is today: sidebar, header, canvas height, strip cards and the terminal drawer. With reduced motion switched on, the replay advances in half-second steps and passing effects are left out.</p>
  </div>
</template>

<style scoped>
.legend :deep(h4) { margin: 1rem 0 0.6rem; font-size: 11.5px; }
.legend dl { margin: 0; display: grid; grid-template-columns: 44px minmax(0, 1fr); gap: 11px 12px; align-items: start; font-size: 13px; }
.legend dt { padding-top: 2px; }
.legend dt svg { display: block; }
.legend dd { margin: 0; }
.legend dd span { color: var(--muted); }
.why { margin: 0 0 0.5rem; padding: 0.8rem 0.95rem; border-radius: var(--radius-md); background: var(--info-soft); font-size: 13px; line-height: 1.5; }
.why p { margin: 0; }
.why p + p { margin-top: 0.5rem; color: var(--muted); }
</style>
