// The redesign mocks: fake data only, no kernel. Run with `scripts/mock.sh` (see its header).
// `#v1` shows the first take (sidebar); anything else the current one (two panels, floating tabs).
import { mount } from 'svelte';
import '../src/app.css';
import ChatMock from './ChatMock.svelte';
import ChatMock2 from './ChatMock2.svelte';

const v1 = location.hash === '#v1';
mount(v1 ? ChatMock : ChatMock2, { target: document.getElementById('app')! });
addEventListener('hashchange', () => location.reload());
