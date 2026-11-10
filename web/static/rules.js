// Colours the Sieve script on the rules page. Built from text nodes, never
// innerHTML, so the script's content cannot inject markup.
const TOKEN =
  /(#[^\n]*|\/\*[\s\S]*?\*\/)|("(?:[^"\\]|\\.)*"|text:[\s\S]*?\n\.\n)|(:[A-Za-z_]\w*)|\b(if|elsif|else|require|stop|allof|anyof|not|true|false|fileinto|redirect|keep|discard|header|address|envelope|exists|size|string|body|date|currentdate|index|duplicate|environment|spamtest|virustest|hasflag|mailboxexists|metadata|metadataexists|servermetadata|servermetadataexists|valid_notify_method|notify_method_capability|valid_ext_list|ihave|vacation|reject|ereject|notify|set|setflag|addflag|removeflag|addheader|deleteheader|replace|enclose|extracttext|foreverypart|break|include|return|global|error|while)\b|(\b\d+[KMG]?\b)/gi;
const CLASSES = [
  "opacity-60 italic",
  "text-success",
  "text-info",
  "text-primary font-semibold",
  "text-warning",
];

const highlight = (source) => {
  const fragment = document.createDocumentFragment();
  let last = 0;
  for (const match of source.matchAll(TOKEN)) {
    fragment.append(source.slice(last, match.index));
    const span = document.createElement("span");
    span.className = CLASSES[match.findIndex((g, i) => i > 0 && g) - 1];
    span.textContent = match[0];
    fragment.append(span);
    last = match.index + match[0].length;
  }
  fragment.append(source.slice(last));
  return fragment;
};

const pre = document.getElementById("rules-source");
if (pre) pre.replaceChildren(highlight(pre.textContent));
