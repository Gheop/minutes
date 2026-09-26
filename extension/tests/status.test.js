// gjs -m extension/tests/status.test.js
import {clock, describe, previewLines} from '../minutes@gheop.github/status.js';

let failed = 0;
function check(what, got, want) {
    const [g, w] = [JSON.stringify(got), JSON.stringify(want)];
    if (g !== w) {
        failed++;
        printerr(`FAIL ${what}: got ${g}, want ${w}`);
    }
}

check('clock under an hour', clock(65), '01:05');
check('clock from an hour on', clock(3725), '1:02:05');
check('clock never negative', clock(-3), '00:00');

check('idle hides the indicator', describe(['idle', 0, 0], 100).visible, false);
check('no state yet hides it too', describe(null, 100).visible, false);

const recording = describe(['recording', 1000, 0], 1754);
check('recording shows the time since the start', recording.label, '12:34');
check('recording ticks', recording.ticking, true);
check('recording can pause and stop', [recording.canPause, recording.canStop], [true, true]);

const paused = describe(['paused', 90, 0], 99999);
check('paused shows the time recorded, not the time since', paused.label, '01:30');
check('paused offers to resume', [paused.paused, paused.ticking], [true, false]);

const transcribing = describe(['transcribing', 0, 0.427], 0);
check('transcribing shows whole percents', transcribing.label, '42 %');
check('a transcript being written cannot be stopped from the bar', transcribing.canStop, false);

const lines = Array.from({length: 20}, (_, i) => [`00:${String(i).padStart(2, '0')}`, i % 2 ? 'Others' : 'You', `line ${i}`]);
check('the preview keeps the newest lines', previewLines(lines).map(l => l.text).at(-1), 'line 19');
check('the preview shows at most twelve by default', previewLines(lines).length, 12);
check('each line gets who and when', previewLines(lines)[0].heading, 'You · 00:08');
check('no preview yet is no lines', previewLines(null), []);

if (failed) {
    printerr(`${failed} failed`);
    imports.system.exit(1);
}
print('status.js: all checks pass');
