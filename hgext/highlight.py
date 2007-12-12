"""
This is Mercurial extension for syntax highlighting in the file
revision view of hgweb.

It depends on the pygments syntax highlighting library:
http://pygments.org/

To enable the extension add this to hgrc:

[extensions]
hgext.highlight =

There is a single configuration option:

[web]
pygments_style = <style>

The default is 'colorful'.  If this is changed the corresponding CSS
file should be re-generated by running

# pygmentize -f html -S <newstyle>


-- Adam Hupp <adam@hupp.org>


"""

from mercurial import demandimport
demandimport.ignore.extend(['pkgutil',
                            'pkg_resources',
                            '__main__',])

import mimetypes

from mercurial.hgweb import hgweb_mod
from mercurial.hgweb.hgweb_mod import hgweb
from mercurial import util
from mercurial.hgweb.common import paritygen
from mercurial.node import hex

from pygments import highlight
from pygments.util import ClassNotFound
from pygments.lexers import guess_lexer_for_filename, TextLexer
from pygments.formatters import HtmlFormatter

SYNTAX_CSS = ('\n<link rel="stylesheet" href="#staticurl#highlight.css" '
              'type="text/css" />')

class StripedHtmlFormatter(HtmlFormatter):
    def __init__(self, stripecount, *args, **kwargs):
        super(StripedHtmlFormatter, self).__init__(*args, **kwargs)
        self.stripecount = stripecount

    def wrap(self, source, outfile):
        yield 0, "<div class='highlight'>"
        yield 0, "<pre>"
        parity = paritygen(self.stripecount)

        for n, i in source:
            if n == 1:
                i = "<div class='parity%s'>%s</div>" % (parity.next(), i)
            yield n, i

        yield 0, "</pre>"
        yield 0, "</div>"


def pygments_format(filename, rawtext, forcetext, encoding,
                    stripecount, style):
    etext = util.tolocal(rawtext)
    if not forcetext:
        try:
            lexer = guess_lexer_for_filename(filename, etext,
                                             encoding=util._encoding)
        except ClassNotFound:
            lexer = TextLexer(encoding=util._encoding)
    else:
        lexer = TextLexer(encoding=util._encoding)

    formatter = StripedHtmlFormatter(stripecount, style=style,
                                     linenos='inline', encoding=encoding)

    return highlight(etext, lexer, formatter)


def filerevision_pygments(self, tmpl, fctx):
    """Reimplement hgweb.filerevision to use syntax highlighting"""
    f = fctx.path()

    rawtext = fctx.data()
    text = rawtext

    fl = fctx.filelog()
    n = fctx.filenode()

    mt = mimetypes.guess_type(f)[0]

    if util.binary(text):
        mt = mt or 'application/octet-stream'
        text = "(binary:%s)" % mt

        # don't parse (binary:...) as anything
        forcetext = True
    else:
        mt = mt or 'text/plain'
        forcetext = False

    def lines(text):
        for line in text.splitlines(True):
            yield {"line": line}

    style = self.config("web", "pygments_style", "colorful")

    text_formatted = lines(pygments_format(f, text, forcetext, self.encoding,
                                           self.stripecount, style))

    # override per-line template
    tmpl.cache['fileline'] = '#line#'

    # append a <link ...> to the syntax highlighting css
    old_header = ''.join(tmpl('header'))
    if SYNTAX_CSS not in old_header:
        new_header =  old_header + SYNTAX_CSS
        tmpl.cache['header'] = new_header

    yield tmpl("filerevision",
               file=f,
               path=hgweb_mod._up(f), # fixme: make public
               text=text_formatted,
               raw=rawtext,
               mimetype=mt,
               rev=fctx.rev(),
               node=hex(fctx.node()),
               author=fctx.user(),
               date=fctx.date(),
               desc=fctx.description(),
               parent=self.siblings(fctx.parents()),
               child=self.siblings(fctx.children()),
               rename=self.renamelink(fl, n),
               permissions=fctx.manifest().flags(f))


# monkeypatch in the new version
# should be safer than overriding the method in a derived class
# and then patching the class
hgweb.filerevision = filerevision_pygments
