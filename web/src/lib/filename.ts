/** One Markdown filename, never a path. */
export function cleanFileName(input: string): string {
  let name = input.trim().replace(/\.md$/i, '');
  name = name.replace(/[\\/:*?"<>|#^\[\]]/g, ' ').replace(/\s+/g, ' ').trim();
  name = name.replace(/^[.\s]+/, '');
  return name ? `${name}.md` : '';
}
