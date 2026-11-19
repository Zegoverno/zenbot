// The chat redesign mock: fake data only, no kernel. Run with `scripts/mock.sh` (see its header).
import { mount } from 'svelte';
import '../src/app.css';
import ChatMock from './ChatMock.svelte';

mount(ChatMock, { target: document.getElementById('app')! });
