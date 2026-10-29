import type { Paper } from "./types";

/**
 * A saved way of narrowing the day's papers.
 *
 * A paper belongs to a topic when it is in one of the topic's categories (any
 * category, if it names none; a name without a dot covers its whole archive)
 * and its title or abstract contains one of the
 * topic's words (any wording, if it names none). Words match at the start of
 * a word, so "interpretab" finds "interpretable" and "interpretability".
 */
export interface Topic {
  name: string;
  categories: string[];
  words: string[];
  builtin?: boolean;
}

/** What the list is showing: one topic or none, plus categories and a search. */
export interface Filter {
  topic: string | null;
  categories: string[];
  query: string;
}

export const EMPTY_FILTER: Filter = { topic: null, categories: [], query: "" };

export const BUILTIN_TOPICS: Topic[] = [
  {
    name: "AI safety",
    builtin: true,
    categories: ["cs.AI", "cs.LG", "cs.CL", "cs.CY", "cs.CR", "cs.HC", "cs.MA", "stat.ML"],
    words: [
      "ai safety",
      "ai alignment",
      "value alignment",
      "safety alignment",
      "llm alignment",
      "alignment of large language",
      "aligning large language",
      "aligning llm",
      "misalign",
      "mechanistic interpretab",
      "sparse autoencoder",
      "steering vector",
      "jailbreak",
      "red team",
      "red-team",
      "reward hacking",
      "rlhf",
      "scalable oversight",
      "deceptive alignment",
      "sycophan",
      "scheming",
      "sandbag",
      "power-seeking",
      "corrigib",
      "situational awareness",
      "model organism",
      "dangerous capabilit",
      "existential risk",
      "catastrophic risk",
      "ai risk",
      "ai governance",
      "llm safety",
      "safety of large language",
      "safety evaluation",
      "safety degradation",
      "harmful content",
      "guardrail",
      "refusal",
    ],
  },
  {
    name: "Animal behavior",
    builtin: true,
    categories: ["q-bio", "physics.bio-ph", "nlin.AO", "cs.CV", "cs.SD", "eess.AS"],
    words: [
      "animal behavio",
      "etholog",
      "foraging",
      "collective behavio",
      "collective motion",
      "flocking",
      "fish school",
      "animal migration",
      "bird migration",
      "birdsong",
      "songbird",
      "animal communication",
      "animal vocali",
      "bioacoustic",
      "predator",
      "courtship",
      "mating behavio",
      "social insect",
      "ant colon",
      "honeybee",
      "primate behavio",
      "zebrafish behavio",
      "insect behavio",
      "bird behavio",
      "rodent behavio",
      "mouse behavio",
      "animal pose",
      "wildlife",
      "camera trap",
      "livestock",
    ],
  },
];

/** arXiv's own names for its archives, for grouping the category list. */
export const ARCHIVE_NAMES: Record<string, string> = {
  "astro-ph": "Astrophysics",
  "cond-mat": "Condensed matter",
  cs: "Computer science",
  econ: "Economics",
  eess: "Electrical engineering and systems science",
  "gr-qc": "General relativity and quantum cosmology",
  "hep-ex": "High energy physics: experiment",
  "hep-lat": "High energy physics: lattice",
  "hep-ph": "High energy physics: phenomenology",
  "hep-th": "High energy physics: theory",
  math: "Mathematics",
  "math-ph": "Mathematical physics",
  nlin: "Nonlinear sciences",
  "nucl-ex": "Nuclear experiment",
  "nucl-th": "Nuclear theory",
  physics: "Physics",
  "q-bio": "Quantitative biology",
  "q-fin": "Quantitative finance",
  "quant-ph": "Quantum physics",
  stat: "Statistics",
};

/** arXiv's names for the categories, as its taxonomy page gives them. */
export const CATEGORY_NAMES: Record<string, string> = {
  "astro-ph.CO": "Cosmology and Nongalactic Astrophysics",
  "astro-ph.EP": "Earth and Planetary Astrophysics",
  "astro-ph.GA": "Astrophysics of Galaxies",
  "astro-ph.HE": "High Energy Astrophysical Phenomena",
  "astro-ph.IM": "Instrumentation and Methods for Astrophysics",
  "astro-ph.SR": "Solar and Stellar Astrophysics",
  "cond-mat.dis-nn": "Disordered Systems and Neural Networks",
  "cond-mat.mes-hall": "Mesoscale and Nanoscale Physics",
  "cond-mat.mtrl-sci": "Materials Science",
  "cond-mat.other": "Other Condensed Matter",
  "cond-mat.quant-gas": "Quantum Gases",
  "cond-mat.soft": "Soft Condensed Matter",
  "cond-mat.stat-mech": "Statistical Mechanics",
  "cond-mat.str-el": "Strongly Correlated Electrons",
  "cond-mat.supr-con": "Superconductivity",
  "cs.AI": "Artificial Intelligence",
  "cs.AR": "Hardware Architecture",
  "cs.CC": "Computational Complexity",
  "cs.CE": "Computational Engineering, Finance, and Science",
  "cs.CG": "Computational Geometry",
  "cs.CL": "Computation and Language",
  "cs.CR": "Cryptography and Security",
  "cs.CV": "Computer Vision and Pattern Recognition",
  "cs.CY": "Computers and Society",
  "cs.DB": "Databases",
  "cs.DC": "Distributed, Parallel, and Cluster Computing",
  "cs.DL": "Digital Libraries",
  "cs.DM": "Discrete Mathematics",
  "cs.DS": "Data Structures and Algorithms",
  "cs.ET": "Emerging Technologies",
  "cs.FL": "Formal Languages and Automata Theory",
  "cs.GL": "General Literature",
  "cs.GR": "Graphics",
  "cs.GT": "Computer Science and Game Theory",
  "cs.HC": "Human-Computer Interaction",
  "cs.IR": "Information Retrieval",
  "cs.IT": "Information Theory",
  "cs.LG": "Machine Learning",
  "cs.LO": "Logic in Computer Science",
  "cs.MA": "Multiagent Systems",
  "cs.MM": "Multimedia",
  "cs.MS": "Mathematical Software",
  "cs.NA": "Numerical Analysis",
  "cs.NE": "Neural and Evolutionary Computing",
  "cs.NI": "Networking and Internet Architecture",
  "cs.OH": "Other Computer Science",
  "cs.OS": "Operating Systems",
  "cs.PF": "Performance",
  "cs.PL": "Programming Languages",
  "cs.RO": "Robotics",
  "cs.SC": "Symbolic Computation",
  "cs.SD": "Sound",
  "cs.SE": "Software Engineering",
  "cs.SI": "Social and Information Networks",
  "cs.SY": "Systems and Control",
  "econ.EM": "Econometrics",
  "econ.GN": "General Economics",
  "econ.TH": "Theoretical Economics",
  "eess.AS": "Audio and Speech Processing",
  "eess.IV": "Image and Video Processing",
  "eess.SP": "Signal Processing",
  "eess.SY": "Systems and Control",
  "math.AC": "Commutative Algebra",
  "math.AG": "Algebraic Geometry",
  "math.AP": "Analysis of PDEs",
  "math.AT": "Algebraic Topology",
  "math.CA": "Classical Analysis and ODEs",
  "math.CO": "Combinatorics",
  "math.CT": "Category Theory",
  "math.CV": "Complex Variables",
  "math.DG": "Differential Geometry",
  "math.DS": "Dynamical Systems",
  "math.FA": "Functional Analysis",
  "math.GM": "General Mathematics",
  "math.GN": "General Topology",
  "math.GR": "Group Theory",
  "math.GT": "Geometric Topology",
  "math.HO": "History and Overview",
  "math.IT": "Information Theory",
  "math.KT": "K-Theory and Homology",
  "math.LO": "Logic",
  "math.MG": "Metric Geometry",
  "math.MP": "Mathematical Physics",
  "math.NA": "Numerical Analysis",
  "math.NT": "Number Theory",
  "math.OA": "Operator Algebras",
  "math.OC": "Optimization and Control",
  "math.PR": "Probability",
  "math.QA": "Quantum Algebra",
  "math.RA": "Rings and Algebras",
  "math.RT": "Representation Theory",
  "math.SG": "Symplectic Geometry",
  "math.SP": "Spectral Theory",
  "math.ST": "Statistics Theory",
  "nlin.AO": "Adaptation and Self-Organizing Systems",
  "nlin.CD": "Chaotic Dynamics",
  "nlin.CG": "Cellular Automata and Lattice Gases",
  "nlin.PS": "Pattern Formation and Solitons",
  "nlin.SI": "Exactly Solvable and Integrable Systems",
  "physics.acc-ph": "Accelerator Physics",
  "physics.ao-ph": "Atmospheric and Oceanic Physics",
  "physics.app-ph": "Applied Physics",
  "physics.atm-clus": "Atomic and Molecular Clusters",
  "physics.atom-ph": "Atomic Physics",
  "physics.bio-ph": "Biological Physics",
  "physics.chem-ph": "Chemical Physics",
  "physics.class-ph": "Classical Physics",
  "physics.comp-ph": "Computational Physics",
  "physics.data-an": "Data Analysis, Statistics and Probability",
  "physics.ed-ph": "Physics Education",
  "physics.flu-dyn": "Fluid Dynamics",
  "physics.gen-ph": "General Physics",
  "physics.geo-ph": "Geophysics",
  "physics.hist-ph": "History and Philosophy of Physics",
  "physics.ins-det": "Instrumentation and Detectors",
  "physics.med-ph": "Medical Physics",
  "physics.optics": "Optics",
  "physics.plasm-ph": "Plasma Physics",
  "physics.pop-ph": "Popular Physics",
  "physics.soc-ph": "Physics and Society",
  "physics.space-ph": "Space Physics",
  "q-bio.BM": "Biomolecules",
  "q-bio.CB": "Cell Behavior",
  "q-bio.GN": "Genomics",
  "q-bio.MN": "Molecular Networks",
  "q-bio.NC": "Neurons and Cognition",
  "q-bio.OT": "Other Quantitative Biology",
  "q-bio.PE": "Populations and Evolution",
  "q-bio.QM": "Quantitative Methods",
  "q-bio.SC": "Subcellular Processes",
  "q-bio.TO": "Tissues and Organs",
  "q-fin.CP": "Computational Finance",
  "q-fin.EC": "Economics",
  "q-fin.GN": "General Finance",
  "q-fin.MF": "Mathematical Finance",
  "q-fin.PM": "Portfolio Management",
  "q-fin.PR": "Pricing of Securities",
  "q-fin.RM": "Risk Management",
  "q-fin.ST": "Statistical Finance",
  "q-fin.TR": "Trading and Market Microstructure",
  "stat.AP": "Applications",
  "stat.CO": "Computation",
  "stat.ME": "Methodology",
  "stat.ML": "Machine Learning",
  "stat.OT": "Other Statistics",
  "stat.TH": "Statistics Theory",
};

export function archiveOf(category: string): string {
  const dot = category.indexOf(".");
  return dot < 0 ? category : category.slice(0, dot);
}

export function categoryName(category: string): string {
  return CATEGORY_NAMES[category] ?? ARCHIVE_NAMES[category] ?? category;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** One compiled pattern per topic, matching any of its words at a word start. */
const patterns = new Map<string, RegExp | null>();

function wordsPattern(words: string[]): RegExp | null {
  const key = words.join("\u0000");
  if (!patterns.has(key)) {
    patterns.set(
      key,
      words.length ? new RegExp(`\\b(?:${words.map(escapeRegExp).join("|")})`, "i") : null,
    );
  }
  return patterns.get(key) ?? null;
}

/** A category given without a dot, such as "q-bio", stands for its whole archive. */
function inAny(p: Paper, categories: string[]): boolean {
  return (
    categories.length === 0 ||
    p.categories.some((c) => categories.some((want) => c === want || c.startsWith(`${want}.`)))
  );
}

export function inTopic(p: Paper, t: Topic): boolean {
  if (!inAny(p, t.categories)) return false;
  const re = wordsPattern(t.words);
  return re === null || re.test(`${p.title}\n${p.abstract}`);
}

/** Every term of the search must appear in the title, abstract, an author or a category. */
function matchesQuery(p: Paper, query: string): boolean {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return true;
  const text = [
    p.title,
    p.abstract,
    ...p.authors.map((a) => a.name),
    ...p.categories,
    ...p.categories.map(categoryName),
  ]
    .join("\n")
    .toLowerCase();
  return terms.every((t) => text.includes(t));
}

export function isActive(f: Filter): boolean {
  return f.topic !== null || f.categories.length > 0 || f.query.trim() !== "";
}

export function applyFilter(papers: Paper[], f: Filter, topics: Topic[]): Paper[] {
  if (!isActive(f)) return papers;
  const topic = f.topic === null ? undefined : topics.find((t) => t.name === f.topic);
  return papers.filter(
    (p) =>
      (topic === undefined || inTopic(p, topic)) &&
      inAny(p, f.categories) &&
      matchesQuery(p, f.query),
  );
}

const FILTER_KEY = "nearfield-filter";
const TOPICS_KEY = "nearfield-topics";

export function loadFilter(): Filter {
  try {
    const raw = localStorage.getItem(FILTER_KEY);
    if (!raw) return { ...EMPTY_FILTER };
    const f = JSON.parse(raw) as Partial<Filter>;
    return {
      topic: typeof f.topic === "string" ? f.topic : null,
      categories: Array.isArray(f.categories) ? f.categories.filter((c) => typeof c === "string") : [],
      query: typeof f.query === "string" ? f.query : "",
    };
  } catch {
    return { ...EMPTY_FILTER };
  }
}

export function saveFilter(f: Filter): void {
  try {
    localStorage.setItem(FILTER_KEY, JSON.stringify(f));
  } catch {
    /* storage unavailable: the filter lasts until the app closes */
  }
}

function userTopics(): Topic[] {
  try {
    const raw = localStorage.getItem(TOPICS_KEY);
    const list = raw ? (JSON.parse(raw) as Topic[]) : [];
    return list.filter(
      (t) => typeof t?.name === "string" && Array.isArray(t.categories) && Array.isArray(t.words),
    );
  } catch {
    return [];
  }
}

function saveUserTopics(list: Topic[]): void {
  try {
    localStorage.setItem(TOPICS_KEY, JSON.stringify(list));
  } catch {
    /* storage unavailable */
  }
}

/** The built-in topics, then the ones saved on this device. */
export function allTopics(): Topic[] {
  const mine = userTopics().filter((t) => !BUILTIN_TOPICS.some((b) => b.name === t.name));
  return [...BUILTIN_TOPICS, ...mine];
}

export function addTopic(t: Topic): void {
  const rest = userTopics().filter((u) => u.name !== t.name);
  saveUserTopics([...rest, { name: t.name, categories: t.categories, words: t.words }]);
}

export function removeTopic(name: string): void {
  saveUserTopics(userTopics().filter((t) => t.name !== name));
}
