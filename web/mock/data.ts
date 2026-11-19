// Fake sessions and messages for the chat mock. Shapes are loose on purpose: this is for layout.

export type Step = { label: string; detail: string; failed?: boolean };
export type Msg =
  | { k: 'user'; text: string; time: string; files?: string[] }
  | { k: 'bot'; html: string; time: string }
  | { k: 'work'; summary: string; steps: Step[] }
  | { k: 'ask'; question: string; options: string[] };

export type State = 'idle' | 'working' | 'waiting';
export type Session = { id: string; title: string; when: string; state: State; sub?: number };

/** Long-lived rooms zenbot also answers in (Matrix, …). */
export const channels: Session[] = [
  { id: 'matrix', title: 'Matrix', when: '2d', state: 'idle' },
];

/** Open jobs: each session is one job, end to end. */
export const sessions: Session[] = [
  { id: 'trip', title: 'Plan a weekend in Campos do Jordão', when: '9:12', state: 'working', sub: 2 },
  { id: 'budget', title: 'Family budget for December', when: '8:40', state: 'waiting' },
  { id: 'gift', title: 'Christmas gift ideas for the parents', when: '7:05', state: 'waiting' },
  { id: 'brief', title: 'Morning brief', when: '6:00', state: 'working' },
  { id: 'budgetwk', title: 'Weekly budget check', when: 'Mon', state: 'idle' },
  { id: 'recipe', title: 'Baby-friendly dinner recipes', when: 'Wed', state: 'idle' },
];

/** Finished jobs; the count is all of them, the list the latest. */
export const archivedCount = 3625;
export const archived: Session[] = [
  { id: 'doctor', title: 'Paediatrician appointment reminders', when: 'Tue', state: 'idle' },
  { id: 'mass', title: 'Sunday Mass times near home', when: 'Mon', state: 'idle' },
  { id: 'car', title: 'Car insurance renewal', when: 'Nov 12', state: 'idle' },
];

export const conversation: Msg[] = [
  { k: 'user', time: '9:12', text: 'Can you plan a weekend in Campos do Jordão for us? Two adults and a baby, leaving Friday after work, back Sunday evening.' },
  {
    k: 'work',
    summary: 'Looked up hotels, weather and driving time',
    steps: [
      { label: 'Searched the web', detail: 'baby-friendly hotels Campos do Jordão' },
      { label: 'Read a page', detail: 'booking.com · Hotel Frontenac' },
      { label: 'Read a page', detail: 'climatempo.com.br · weekend forecast' },
      { label: 'Checked the calendar', detail: 'Fri 21 – Sun 23 Nov', failed: false },
    ],
  },
  {
    k: 'bot',
    time: '9:13',
    html: `<p>Here's a calm plan that keeps the baby's naps in mind:</p>
<p><strong>Friday</strong> — leave São Paulo by 18:30; it's about 2h45 by car. Dinner at the hotel.</p>
<p><strong>Saturday</strong> — morning walk at Horto Florestal (stroller-friendly paths), lunch at Baden Baden, nap back at the hotel, sunset at Morro do Elefante.</p>
<p><strong>Sunday</strong> — Mass at Igreja de Santa Terezinha at 9:00, slow lunch, leave by 15:00 to beat traffic.</p>
<p>It'll be cold at night (8 °C), so pack layers for the baby.</p>`,
  },
  { k: 'user', time: '9:15', text: 'Love it. Which hotel would you pick?' },
  {
    k: 'work',
    summary: 'Compared 3 hotels',
    steps: [
      { label: 'Read a page', detail: 'Hotel Frontenac · rooms and prices' },
      { label: 'Read a page', detail: 'Pousada Villa Bella · reviews' },
      { label: 'Read a page', detail: 'Grande Hotel Campos do Jordão · family rooms', failed: true },
    ],
  },
  {
    k: 'bot',
    time: '9:16',
    html: `<p>I'd go with <strong>Pousada Villa Bella</strong>: cribs on request, quiet rooms, and a heated pool. About R$ 890 a night.</p>
<p>The Grande Hotel's site didn't load, so I couldn't compare it fairly.</p>`,
  },
];

export const question: Msg = {
  k: 'ask',
  question: 'Should I book Pousada Villa Bella for Fri 21 – Sun 23?',
  options: ['Yes, book it', 'Show me other options', 'Not now'],
};

export const suggestions = [
  'Plan our week',
  'Help me write a message',
  'Find a recipe for tonight',
  'Remind me of something',
];

/** A fake filesystem for the Files tab: folders hold children; files carry a short "when". */
export type FsNode = { name: string; when?: string; kids?: FsNode[] };
const d = (name: string, kids: FsNode[] = [], when?: string): FsNode => ({ name, kids, when });
const f = (name: string, when: string): FsNode => ({ name, when });
export const VAULT = '/home/exedev/.zenbot/global';
export const fsRoot: FsNode = d('', [
  d('bin'), d('etc', [f('hostname', 'Oct 2'), f('hosts', 'Oct 2')]), d('tmp', [f('a1.png', '9:20')]), d('usr', [d('bin'), d('lib'), d('share')]), d('var', [d('log')]),
  d('home', [
    d('exedev', [
      d('.zenbot', [
        d('global', [
          d('Daily', [f('2026-11-19.md', '6:02'), f('2026-11-18.md', 'Wed'), f('2026-11-17.md', 'Tue')], 'today'),
          d('Goals', [f('A present and courageous family life.md', 'Nov 3'), f('Friendship with God and faithful stewardship.md', 'Nov 3'), f('Rebuild a sustainable athletic life.md', 'Oct 30')], 'Nov 3'),
          d('Projects', [d('Baby room', [f('baby-room-ideas.png', 'Wed'), f('Shopping list.md', 'Wed')], 'Wed'), d('Campos do Jordão', [f('Campos do Jordão trip.md', '9:16'), f('Packing list.md', '9:20')], '9:20'), d('Family budget', [f('Family budget — December.md', '8:41'), f('2026 spending.csv', 'Mon')], '8:41')], '9:20'),
          d('Templates', [f('Daily.md', 'Sep 12'), f('Project.md', 'Sep 12')], 'Sep 12'),
          d('wiki', [f('Advent.md', 'Nov 16'), f('Baby sleep.md', 'Nov 10'), f('Budgeting.md', 'Oct 22'), f('Pousada Villa Bella.md', '9:16')], '9:16'),
          f('AGENTS.md', 'Nov 2'), f('MEMORY.md', '3:00'),
        ], '9:20'),
        d('agents', [d('zenbot', [f('IDENTITY.md', 'Nov 18'), f('SOUL.md', 'Oct 8')])]), d('outputs'),
        f('AGENTS.md', 'Nov 12'), f('USER.md', 'Nov 18'),
      ], '9:20'),
      d('inbox', [d('2026-11-19', [f('preview-1795105260571.png', '9:41')])], '9:41'),
      d('zenbot', [d('crates'), d('web'), f('AGENTS.md', 'Nov 17'), f('README.md', 'Nov 17')], 'Nov 18'),
      d('zengame', [f('project.godot', 'Nov 19')], 'Nov 19'),
      f('.bashrc', 'Sep 1'),
    ], '9:41'),
  ]),
]);

/** Note text by vault-relative path; other notes get a short stand-in. */
export const NOTES: Record<string, string> = {
  // Outside the vault: keyed by full path. USER.md is shared by every agent (~/.zenbot/USER.md).
  '/home/exedev/.zenbot/USER.md': `# USER.md — the owner

## Who I am

Adriane, 30, São Paulo. Catholic. Married to José; first son, José, born 2026.

## How I like to work

- Portuguese at home, English is fine for work.
- Short answers; ask before spending money or messaging anyone.
- Mornings are for the baby; I check things after lunch.

## What I'm working on

- Baby room, the family budget and a weekend in Campos do Jordão.
`,
  'Projects/Campos do Jordão/Campos do Jordão trip.md': `---
type: project
status: active
tags: [family, travel]
when: Fri 21 – Sun 23 Nov
---
# Campos do Jordão trip

A calm weekend away, the first with the baby. Two adults and José (6 months), leaving Friday after work.

## Plan

**Friday:** leave São Paulo by 18:30; about 2h45 by car. Dinner at the hotel.

**Saturday:** morning walk at Horto Florestal (stroller-friendly paths), lunch at Baden Baden, nap back at the hotel, sunset at Morro do Elefante.

**Sunday:** Mass at Igreja de Santa Terezinha at 9:00, slow lunch, leave by 15:00 to beat traffic.

## Where we stay

[[Pousada Villa Bella]]: family suite with a crib, R$ 1.780 for both nights. Free cancellation until Wednesday.

## To do

- [x] Pick the hotel
- [x] Book Pousada Villa Bella
- [ ] Ask Mom to water the plants
- [ ] Pack: see [[Packing list]]
- [ ] Fill the tank on Thursday

> It'll be cold at night (8 °C), so pack layers for the baby.
`,
  'Projects/Campos do Jordão/Packing list.md': `# Packing list

## Baby
- [x] Stroller
- [ ] Warm onesies (3)
- [ ] Hat and socks
- [ ] Nappies for 3 days
- [ ] White noise speaker

## Us
- [ ] Coats and scarves
- [ ] Walking shoes
- [ ] Rosary for Sunday

Back to [[Campos do Jordão trip]].
`,
  'Projects/Family budget/Family budget — December.md': `---
type: project
tags: [family, money]
---
# Family budget — December

December runs **R$ 400 short** because of Christmas gifts and the trip. zenbot suggested moving R$ 400 from savings.

| Item | Planned | Spent |
|---|---:|---:|
| Groceries | 2.400 | 2.590 |
| Baby | 900 | 860 |
| Gifts | 1.200 | 640 |
| Trip | 1.800 | 1.780 |
| **Total** | **6.300** | **5.870** |

## Notes
- Groceries are 8% over; mostly the Christmas shopping.
- Gifts list lives in the chat *Christmas gift ideas for the parents*.
`,
  'wiki/Pousada Villa Bella.md': `---
type: place
tags: [travel, campos-do-jordao]
---
# Pousada Villa Bella

A quiet pousada in Campos do Jordão, 10 minutes from Capivari. Good with babies: cribs on request, quiet rooms, a heated pool.

- **Price:** about R$ 890 a night (family suite)
- **Booked:** Fri 21 – Sun 23 Nov, see [[Campos do Jordão trip]]
- **Site:** https://pousadavillabella.com.br
`,
  'Daily/2026-11-19.md': `# Thursday 19 November

## Today
- [x] Pay the electricity bill
- [ ] Call Mom: her birthday, 12:00
- [ ] Pick up the dry cleaning, 18:00

## Gospel
Lk 19:41–44: *“If this day you only knew what makes for peace.”*
`,
};

/** Properties of notes that have no text above (as if read from their front matter). */
export const PROPS: Record<string, Record<string, string[]>> = {
  'Daily/2026-11-18.md': { type: ['daily'] },
  'Daily/2026-11-17.md': { type: ['daily'] },
  'Daily/2026-11-19.md': { type: ['daily'] },
  'Goals/A present and courageous family life.md': { type: ['goal'], status: ['active'], tags: ['family'] },
  'Goals/Friendship with God and faithful stewardship.md': { type: ['goal'], status: ['active'], tags: ['faith', 'money'] },
  'Goals/Rebuild a sustainable athletic life.md': { type: ['goal'], status: ['paused'], tags: ['health'] },
  'Projects/Baby room/Shopping list.md': { type: ['project'], status: ['done'], tags: ['family', 'baby'] },
  'Projects/Campos do Jordão/Packing list.md': { type: ['project'], status: ['active'], tags: ['travel', 'baby'] },
  'wiki/Advent.md': { type: ['concept'], tags: ['faith'] },
  'wiki/Baby sleep.md': { type: ['concept'], tags: ['baby', 'family'] },
  'wiki/Budgeting.md': { type: ['solution'], tags: ['money'] },
  'Templates/Daily.md': { type: ['template'] },
  'Templates/Project.md': { type: ['template'] },
};

/** Where chat attachments land (web/server/uploads.ts): ~/inbox/<day>/, never overwriting. */
export const INBOX = '/home/exedev/inbox';
/** Picked files' bytes in this tab (object URLs), so the mock can show an attached image. */
export const BLOBS: Record<string, string> = {};
export function addToInbox(name: string, day = '2026-11-19', url?: string): string {
  const home = fsRoot.kids!.find((k) => k.name === 'home')!.kids!.find((k) => k.name === 'exedev')!;
  const inbox = home.kids!.find((k) => k.name === 'inbox')!;
  let dir = inbox.kids!.find((k) => k.name === day);
  if (!dir) inbox.kids!.push((dir = d(day, [], 'now')));
  const dot = name.lastIndexOf('.');
  const [stem, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ''];
  let n = name;
  for (let i = 2; dir.kids!.some((k) => k.name === n); i++) n = `${stem}-${i}${ext}`;
  dir.kids!.push(f(n, 'now'));
  if (url) BLOBS[`${INBOX}/${day}/${n}`] = url;
  return `${INBOX}/${day}/${n}`;
}
