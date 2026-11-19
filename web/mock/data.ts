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
