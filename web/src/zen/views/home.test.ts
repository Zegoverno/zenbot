import { describe, expect, it } from 'vitest';
import { defaults, layoutOf, selectTasks, slot, type LiveTask } from './home.ts';

describe('Home layouts and task views', () => {
  it('defaults safely and preserves an empty or customized layout', () => {
    expect(layoutOf(undefined)).toEqual(defaults());
    expect(layoutOf([{id:'bad',size:1}])).toEqual(defaults());
    expect(layoutOf([])).toEqual([]);
    const layout = [slot('today','a'),slot('today','b')];
    layout[1].filter.show='all';
    expect(layoutOf(layout)).toEqual(layout);
  });
  it('filters by date, project, goal, priority and sorts', () => {
    const t = (text: string, due: string, project: string, priority: LiveTask['priority'], goals: string[] = []) => ({text,due,project,priority,goals,done:false} as LiveTask);
    const tasks=[t('later','2026-11-25','A','urgent',['G']),t('today','2026-11-19','A','important',['G']),t('other','2026-11-18','B','urgent')];
    const f=slot('today').filter;
    expect(selectTasks(tasks,f,'2026-11-19').map(x=>x.text)).toEqual(['other','today']);
    f.show='week';f.project='A';f.goal='G';f.order='priority';
    expect(selectTasks(tasks,f,'2026-11-19').map(x=>x.text)).toEqual(['later','today']);
    f.priority='important';expect(selectTasks(tasks,f,'2026-11-19').map(x=>x.text)).toEqual(['today']);
  });
});
