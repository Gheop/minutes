// What the top bar shows for a state of Minutes, kept apart from the Shell so
// it can be tested with plain gjs.
//
// Minutes publishes its state over D-Bus as (state, seconds, progress):
//   ['idle', 0, 0]
//   ['recording', startedAt, 0]   startedAt: Unix seconds, pauses left out
//   ['paused', recorded, 0]       recorded: seconds recorded so far
//   ['transcribing', 0, progress] progress: 0 to 1

/** `mm:ss`, or `h:mm:ss` from an hour on. */
export function clock(seconds) {
    const s = Math.max(0, Math.floor(seconds));
    const h = Math.floor(s / 3600);
    const m = Math.floor(s / 60) % 60;
    const pad = n => String(n).padStart(2, '0');
    return h > 0 ? `${h}:${pad(m)}:${pad(s % 60)}` : `${pad(m)}:${pad(s % 60)}`;
}

/**
 * How the indicator looks for a state, `now` being the current Unix time in
 * seconds. Nothing shows while Minutes is idle or not running.
 */
export function describe(status, now) {
    const [state, seconds, progress] = status ?? ['idle', 0, 0];
    switch (state) {
    case 'recording':
        return {visible: true, icon: 'media-record-symbolic', label: clock(now - seconds),
            recording: true, canPause: true, paused: false, canStop: true, ticking: true};
    case 'paused':
        return {visible: true, icon: 'media-playback-pause-symbolic', label: clock(seconds),
            recording: false, canPause: true, paused: true, canStop: true, ticking: false};
    case 'transcribing':
        return {visible: true, icon: 'document-edit-symbolic', label: `${Math.floor(progress * 100)} %`,
            recording: false, canPause: false, paused: false, canStop: false, ticking: false};
    default:
        return {visible: false, icon: 'media-record-symbolic', label: '',
            recording: false, canPause: false, paused: false, canStop: false, ticking: false};
    }
}

/**
 * The preview lines to show in the menu, newest last: the last `max` of the
 * (time, speaker, text) lines Minutes publishes, with a heading for each.
 */
/** Seconds from a `clock()` time: "05:13" or "1:05:13". */
function seconds(time) {
    return String(time).split(':').reduce((total, part) => total * 60 + Number(part), 0);
}

/**
 * The newest paragraphs of the preview. Minutes cuts long speech into pieces
 * of a few seconds; the pieces one person says in a row, each less than
 * `gap` seconds after the one before, read as one paragraph.
 */
export function previewLines(lines, max = 12, gap = 30) {
    const paragraphs = [];
    let last = null;
    for (const [time, speaker, text] of lines ?? []) {
        const at = seconds(time);
        if (last && last.speaker === speaker && at - last.at <= gap) {
            last.text += ` ${text}`;
            last.at = at;
            continue;
        }
        last = {speaker, at, heading: `${speaker} · ${time}`, text};
        paragraphs.push(last);
    }
    return paragraphs.slice(-max).map(({heading, text}) => ({heading, text}));
}
