// One shared one-second clock for live timers (the node footers' elapsed time), so a graph full
// of running nodes ticks off a single interval, and nothing ticks once no timer is live.
import { onScopeDispose, readonly, ref, watch, type Ref } from 'vue';

const now = ref(Date.now());
let subscribers = 0;
let timer: ReturnType<typeof setInterval> | undefined;

function subscribe() {
    if (subscribers++ === 0) {
        now.value = Date.now();
        timer = setInterval(() => (now.value = Date.now()), 1000);
    }
}

function unsubscribe() {
    if (--subscribers === 0) {
        clearInterval(timer);
        timer = undefined;
    }
}

/** The current time in ms, ticking every second while `live()` is true. */
export function useNow(live: () => boolean): Readonly<Ref<number>> {
    let ticking = false;
    const set = (want: boolean) => {
        if (want === ticking) return;
        ticking = want;
        if (want) subscribe();
        else unsubscribe();
    };
    watch(live, set, { immediate: true });
    onScopeDispose(() => set(false));
    return readonly(now);
}

/** Parses a backend timestamp; one without an offset is UTC, as the backend stores it. */
export function parseTimestamp(value: string | null | undefined): number | null {
    if (!value) return null;
    const ms = Date.parse(/(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`);
    return Number.isNaN(ms) ? null : ms;
}

/** A coarse age: "just now", "12m", "3h", "2d". */
export function formatAge(ms: number): string {
    const minutes = Math.floor(Math.max(0, ms) / 60_000);
    if (minutes < 1) return 'just now';
    if (minutes < 60) return `${minutes}m`;
    const hours = Math.floor(minutes / 60);
    return hours < 24 ? `${hours}h` : `${Math.floor(hours / 24)}d`;
}

/** "04:12", or "1:02:03" past an hour. */
export function formatElapsed(ms: number): string {
    const total = Math.max(0, Math.floor(ms / 1000));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    const pad = (n: number) => String(n).padStart(2, '0');
    return h ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}
