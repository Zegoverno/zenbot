// Markdown for the page, built straight on Lezer's parser. @codemirror/lang-markdown would
// bring the HTML, CSS and JavaScript languages along (for HTML blocks and code fences), which
// more than doubles the page's size for nothing we use.
import { parser, GFM } from '@lezer/markdown';
import { Language, LanguageSupport, defineLanguageFacet } from '@codemirror/language';

const md = new Language(defineLanguageFacet({ commentTokens: { block: { open: '<!--', close: '-->' } } }), parser.configure(GFM), [], 'markdown');

export function markdown() {
  return new LanguageSupport(md);
}
