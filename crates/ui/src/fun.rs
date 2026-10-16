//! Playful one-liners sprinkled through the interface.
//!
//! They are a setting (on by default, "Playful messages" in the Library
//! menu) and are always off while discreet mode, share-safe mode or the
//! disguised identity is active: nothing cheeky may show up on a shared
//! screen. Lines are mild and translated per language; the pick rotates with
//! a counter so the same moment does not always say the same thing.
//!
//! This is the only module allowed to be funny at runtime. The others have to
//! settle for comments.

use crate::i18n::{self, Lang};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static TICK: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quip {
    Welcome,
    AgeGate,
    Scanned,
    EmptyResults,
    Buffering,
    Ended,
    NothingDownloading,
    InboxZero,
    OneHand,
    ViewSaved,
    Starting,
    Downloaded,
}

/// Called every frame from the app with the effective state.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A line for this moment, or `None` when jokes are off.
pub fn quip(q: Quip) -> Option<&'static str> {
    if !enabled() {
        return None;
    }
    let lines = lines(i18n::current(), q);
    if lines.is_empty() {
        return None;
    }
    let n = TICK.fetch_add(1, Ordering::Relaxed);
    Some(lines[n % lines.len()])
}

/// A stable line (no rotation) for text drawn every frame.
pub fn steady(q: Quip) -> Option<&'static str> {
    if !enabled() {
        return None;
    }
    let l = lines(i18n::current(), q);
    if l.is_empty() {
        return None;
    }
    Some(l[session_seed() % l.len()])
}

/// One number per launch, so text drawn every frame keeps its line while a
/// new launch may pick another one.
fn session_seed() -> usize {
    static SEED: OnceLock<usize> = OnceLock::new();
    *SEED.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0)
    })
}

pub fn lines(l: Lang, q: Quip) -> &'static [&'static str] {
    use Quip::*;
    match (l, q) {
        // ---- English ----------------------------------------------------
        (Lang::En, Welcome) => &[
            "Take your time. Nobody's judging.",
            "Make yourself comfortable.",
            "Your secret is safe. Probably.",
            "Nothing here yet. Suspiciously wholesome.",
            "An empty library. Your browser history thanks you.",
            "Ctrl+O to import. We'll call it holiday photos.",
        ],
        (Lang::En, AgeGate) => &["Adults only. No fibbing, you little rascal."],
        (Lang::En, Scanned) => &[
            "Found some new friends.",
            "Fresh content. Naughty, naughty.",
            "More to explore, you little rascal.",
            "Indexed. We didn't touch anything. Can't speak for you.",
            "Those \"Linux ISOs\" were not ISOs.",
        ],
        (Lang::En, EmptyResults) => &[
            "Nothing here. Try something else, naughty one.",
            "Hmmm… nothing matches. Get creative.",
            "No match. Your standards are too high.",
            "No match. Search forgives accents, not imagination.",
        ],
        (Lang::En, Buffering) => &[
            "Hmmm… good things come to those who wait.",
            "Patience, little rascal…",
            "Warming up…",
            "Loading. Still faster than cargo build.",
        ],
        (Lang::En, Ended) => &[
            "Again? You little rascal.",
            "Already? Hit replay.",
            "Shall we go again?",
        ],
        (Lang::En, NothingDownloading) => {
            &["Nothing downloading. Hands free for now.", "Quiet in here…"]
        }
        (Lang::En, InboxZero) => &[
            "Inbox zero. Productive little rascal.",
            "Inbox zero. A productivity win you can tell no one.",
        ],
        (Lang::En, OneHand) => &[
            "Hmmm… why only one hand?",
            "Left-hand mode. We don't ask why.",
        ],
        (Lang::En, ViewSaved) => &["Saved. Your secret is safe with us."],
        (Lang::En, Downloaded) => &[
            "Downloaded. Delivered discreetly, in plain packaging.",
            "Download complete. It's for the codec, we know.",
        ],
        (Lang::En, Starting) => &[
            "Hold on, rascal…",
            "Getting things ready…",
            "Compiling… no, wait, that's just anticipation.",
        ],
        // ---- Français -----------------------------------------------------
        (Lang::Fr, Welcome) => &[
            "Prends ton temps, mon petit coquinou.",
            "Installe-toi bien. Personne ne juge.",
            "Ton secret est bien gardé. Normalement.",
            "Rien ici pour l'instant. C'est presque suspect.",
            "Bibliothèque vide. Ton historique de navigation te remercie.",
            "Ctrl+O pour importer. On dira des photos de vacances.",
        ],
        (Lang::Fr, AgeGate) => &["Réservé aux grands. Pas de mensonge, petit coquin."],
        (Lang::Fr, Scanned) => &[
            "Du nouveau contenu, mon petit coquinou va.",
            "Des nouveautés… vilain garnement.",
            "Encore plus à explorer, petit coquin.",
            "Indexé. Nous, on n'a touché à rien.",
            "Les « ISO Linux » n'en étaient pas.",
        ],
        (Lang::Fr, EmptyResults) => &[
            "Rien par ici. Essaie autre chose, petit coquin.",
            "Hmmmm… rien ne correspond. Sois créatif.",
            "Aucun résultat. Tu es trop exigeant.",
            "Aucun résultat. Insensible aux accents, pas aux fautes de goût.",
        ],
        (Lang::Fr, Buffering) => &[
            "Hmmmm… patience, mon petit coquinou.",
            "Tout vient à point à qui sait attendre…",
            "Ça chauffe…",
            "Chargement. Toujours plus rapide qu'un cargo build.",
        ],
        (Lang::Fr, Ended) => &[
            "Encore ? Petit coquin va.",
            "Déjà fini ? Relance, coquinou.",
            "On remet ça ?",
        ],
        (Lang::Fr, NothingDownloading) => &[
            "Rien en cours. Les mains sont libres.",
            "Calme plat par ici…",
        ],
        (Lang::Fr, InboxZero) => &[
            "Boîte vide. Bravo, petit coquin productif.",
            "Boîte vide. Enfin un KPI à ne montrer à personne.",
        ],
        (Lang::Fr, OneHand) => &[
            "Hmmmm… pourquoi une seule main ?",
            "Mode main gauche. On ne pose pas de questions.",
        ],
        (Lang::Fr, ViewSaved) => &["Enregistré. Ton secret est bien gardé."],
        (Lang::Fr, Downloaded) => &[
            "Téléchargé. Livré discrètement, sous emballage neutre.",
            "Téléchargement terminé. C'est pour le codec, on sait.",
        ],
        (Lang::Fr, Starting) => &[
            "Un instant, coquinou…",
            "On prépare tout ça…",
            "Compilation… ah non, c'est juste l'impatience.",
        ],
        // ---- Español ------------------------------------------------------
        (Lang::Es, Welcome) => &[
            "Tómate tu tiempo, pillín.",
            "Ponte cómodo. Nadie te juzga.",
            "Tu secreto está a salvo. Probablemente.",
            "Nada por aquí todavía. Sospechosamente sano.",
            "Biblioteca vacía. Tu historial de navegación lo agradece.",
            "Ctrl+O para importar. Diremos que son fotos de vacaciones.",
        ],
        (Lang::Es, AgeGate) => &["Solo para mayores. Sin mentiras, pillín."],
        (Lang::Es, Scanned) => &[
            "Contenido nuevo. Qué travieso.",
            "Novedades… granuja.",
            "Más por explorar, pillín.",
            "Indexado. Nosotros no tocamos nada.",
            "Esas «ISO de Linux» no eran ISO.",
        ],
        (Lang::Es, EmptyResults) => &[
            "Aquí no hay nada. Prueba otra cosa, travieso.",
            "Hmmm… nada coincide. Sé creativo.",
            "Sin resultados. Eres muy exigente.",
            "Sin resultados. Perdona acentos, no la falta de imaginación.",
        ],
        (Lang::Es, Buffering) => &[
            "Hmmm… paciencia, pillín.",
            "Lo bueno se hace esperar…",
            "Calentando motores…",
            "Cargando. Aún más rápido que un cargo build.",
        ],
        (Lang::Es, Ended) => &[
            "¿Otra vez? Qué pillín.",
            "¿Ya? Dale a repetir.",
            "¿Repetimos?",
        ],
        (Lang::Es, NothingDownloading) => {
            &["Nada descargando. Manos libres por ahora.", "Qué silencio…"]
        }
        (Lang::Es, InboxZero) => &[
            "Bandeja vacía. Qué pillín tan productivo.",
            "Bandeja vacía. Un logro que no puedes contar.",
        ],
        (Lang::Es, OneHand) => &[
            "Hmmm… ¿por qué solo una mano?",
            "Modo mano izquierda. No preguntamos.",
        ],
        (Lang::Es, ViewSaved) => &["Guardado. Tu secreto está a salvo."],
        (Lang::Es, Downloaded) => &[
            "Descargado. Entrega discreta, en embalaje neutro.",
            "Descarga completa. Es por el códec, ya lo sabemos.",
        ],
        (Lang::Es, Starting) => &[
            "Un momento, pillín…",
            "Preparando todo…",
            "Compilando… no, es solo la impaciencia.",
        ],
        // ---- Deutsch ------------------------------------------------------
        (Lang::De, Welcome) => &[
            "Lass dir Zeit, du kleiner Schlingel.",
            "Mach es dir bequem. Niemand urteilt.",
            "Dein Geheimnis ist sicher. Wahrscheinlich.",
            "Noch nichts hier. Verdächtig harmlos.",
            "Leere Bibliothek. Dein Browserverlauf dankt es dir.",
            "Strg+O zum Importieren. Wir sagen: Urlaubsfotos.",
        ],
        (Lang::De, AgeGate) => &["Nur für Erwachsene. Nicht schummeln, Schlingel."],
        (Lang::De, Scanned) => &[
            "Neue Inhalte. Du kleiner Schlingel.",
            "Frischer Stoff… na sowas.",
            "Mehr zu entdecken, du Racker.",
            "Indiziert. Wir haben nichts angefasst.",
            "Diese „Linux-ISOs“ waren keine ISOs.",
        ],
        (Lang::De, EmptyResults) => &[
            "Hier ist nichts. Probier etwas anderes, Schlingel.",
            "Hmmm… nichts passt. Sei kreativ.",
            "Kein Treffer. Du bist zu anspruchsvoll.",
            "Kein Treffer. Akzente verzeihen wir, Fantasielosigkeit nicht.",
        ],
        (Lang::De, Buffering) => &[
            "Hmmm… Geduld, du Racker.",
            "Gut Ding will Weile haben…",
            "Es wird warm…",
            "Lädt. Immer noch schneller als cargo build.",
        ],
        (Lang::De, Ended) => &[
            "Schon wieder? Du Racker.",
            "Schon vorbei? Nochmal abspielen.",
            "Noch eine Runde?",
        ],
        (Lang::De, NothingDownloading) => {
            &["Nichts im Download. Hände frei.", "Es ist still hier…"]
        }
        (Lang::De, InboxZero) => &[
            "Eingang leer. Fleißiger kleiner Schlingel.",
            "Eingang leer. Ein Erfolg, den du keinem erzählst.",
        ],
        (Lang::De, OneHand) => &[
            "Hmmm… warum nur eine Hand?",
            "Linkshändermodus. Wir fragen nicht.",
        ],
        (Lang::De, ViewSaved) => &["Gespeichert. Dein Geheimnis ist sicher."],
        (Lang::De, Downloaded) => &[
            "Geladen. Diskret geliefert, neutral verpackt.",
            "Download fertig. Ist ja für den Codec, wir wissen es.",
        ],
        (Lang::De, Starting) => &[
            "Moment, Schlingel…",
            "Wird vorbereitet…",
            "Kompiliere… nein, das ist nur Vorfreude.",
        ],
        // ---- Italiano -----------------------------------------------------
        (Lang::It, Welcome) => &[
            "Prenditi il tuo tempo, birbantello.",
            "Mettiti comodo. Nessuno giudica.",
            "Il tuo segreto è al sicuro. Forse.",
            "Ancora niente qui. Sospettosamente innocente.",
            "Libreria vuota. La cronologia del browser ringrazia.",
            "Ctrl+O per importare. Diremo foto delle vacanze.",
        ],
        (Lang::It, AgeGate) => &["Solo per adulti. Niente bugie, birbantello."],
        (Lang::It, Scanned) => &[
            "Nuovi contenuti. Che monello.",
            "Novità fresche… birichino.",
            "Altro da esplorare, birbantello.",
            "Indicizzato. Noi non abbiamo toccato nulla.",
            "Quelle «ISO di Linux» non erano ISO.",
        ],
        (Lang::It, EmptyResults) => &[
            "Qui non c'è niente. Prova altro, monello.",
            "Hmmm… niente corrisponde. Sii creativo.",
            "Nessun risultato. Sei troppo esigente.",
            "Nessun risultato. Perdona gli accenti, non la fantasia.",
        ],
        (Lang::It, Buffering) => &[
            "Hmmm… pazienza, birbantello.",
            "Le cose belle si fanno attendere…",
            "Si scalda…",
            "Caricamento. Ancora più veloce di un cargo build.",
        ],
        (Lang::It, Ended) => &[
            "Ancora? Che monello.",
            "Già finito? Premi riproduci.",
            "Rifacciamo?",
        ],
        (Lang::It, NothingDownloading) => {
            &["Nessun download. Mani libere per ora.", "Che silenzio…"]
        }
        (Lang::It, InboxZero) => &[
            "Posta vuota. Che birbante produttivo.",
            "Posta vuota. Un successo da non raccontare a nessuno.",
        ],
        (Lang::It, OneHand) => &[
            "Hmmm… perché una mano sola?",
            "Modalità mano sinistra. Non chiediamo.",
        ],
        (Lang::It, ViewSaved) => &["Salvato. Il tuo segreto è al sicuro."],
        (Lang::It, Downloaded) => &[
            "Scaricato. Consegna discreta, in confezione neutra.",
            "Download completo. È per il codec, lo sappiamo.",
        ],
        (Lang::It, Starting) => &[
            "Un attimo, birbantello…",
            "Prepariamo tutto…",
            "Compilazione… no, è solo impazienza.",
        ],
        // ---- Português ----------------------------------------------------
        (Lang::Pt, Welcome) => &[
            "Toma o teu tempo, marotinho.",
            "Fica à vontade. Ninguém julga.",
            "O teu segredo está seguro. Provavelmente.",
            "Ainda nada por aqui. Suspeitosamente inocente.",
            "Biblioteca vazia. O teu histórico agradece.",
            "Ctrl+O para importar. Dizemos que são fotos de férias.",
        ],
        (Lang::Pt, AgeGate) => &["Só para adultos. Sem mentiras, marotinho."],
        (Lang::Pt, Scanned) => &[
            "Conteúdo novo. Que maroto.",
            "Novidades… traquinas.",
            "Mais para explorar, marotinho.",
            "Indexado. Nós não tocámos em nada.",
            "Aquelas «ISO de Linux» não eram ISO.",
        ],
        (Lang::Pt, EmptyResults) => &[
            "Aqui não há nada. Tenta outra coisa, maroto.",
            "Hmmm… nada corresponde. Sê criativo.",
            "Sem resultados. És muito exigente.",
            "Sem resultados. Perdoa acentos, não a falta de imaginação.",
        ],
        (Lang::Pt, Buffering) => &[
            "Hmmm… paciência, marotinho.",
            "O que é bom faz-se esperar…",
            "A aquecer…",
            "A carregar. Ainda mais rápido que um cargo build.",
        ],
        (Lang::Pt, Ended) => &[
            "Outra vez? Que maroto.",
            "Já acabou? Carrega em repetir.",
            "Repetimos?",
        ],
        (Lang::Pt, NothingDownloading) => {
            &["Nada a transferir. Mãos livres por agora.", "Que silêncio…"]
        }
        (Lang::Pt, InboxZero) => &[
            "Caixa vazia. Que maroto produtivo.",
            "Caixa vazia. Uma vitória que não podes contar.",
        ],
        (Lang::Pt, OneHand) => &[
            "Hmmm… porquê só uma mão?",
            "Modo mão esquerda. Não perguntamos.",
        ],
        (Lang::Pt, ViewSaved) => &["Guardado. O teu segredo está seguro."],
        (Lang::Pt, Downloaded) => &[
            "Transferido. Entrega discreta, em embalagem neutra.",
            "Transferência concluída. É pelo codec, sabemos.",
        ],
        (Lang::Pt, Starting) => &[
            "Um instante, marotinho…",
            "A preparar tudo…",
            "A compilar… não, é só ansiedade.",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_QUIPS: [Quip; 12] = [
        Quip::Welcome,
        Quip::AgeGate,
        Quip::Scanned,
        Quip::EmptyResults,
        Quip::Buffering,
        Quip::Ended,
        Quip::NothingDownloading,
        Quip::InboxZero,
        Quip::OneHand,
        Quip::ViewSaved,
        Quip::Starting,
        Quip::Downloaded,
    ];

    #[test]
    fn every_language_has_a_line_for_every_moment() {
        for l in i18n::ALL {
            for q in ALL_QUIPS {
                let v = lines(l, q);
                assert!(!v.is_empty(), "{}: {q:?}", l.code());
                assert!(v.iter().all(|s| !s.trim().is_empty() && s.len() < 90));
            }
        }
    }

    #[test]
    fn every_language_has_the_same_number_of_lines_and_they_are_short() {
        for q in ALL_QUIPS {
            let n = lines(Lang::En, q).len();
            for l in i18n::ALL {
                let v = lines(l, q);
                assert_eq!(v.len(), n, "{}: {q:?}", l.code());
                assert!(
                    v.iter().all(|s| s.chars().count() < 80),
                    "{}: {q:?}",
                    l.code()
                );
            }
        }
    }

    #[test]
    fn the_new_lines_exist_in_all_catalogs() {
        let new = [
            (Quip::Welcome, "Nothing here yet. Suspiciously wholesome."),
            (
                Quip::Welcome,
                "An empty library. Your browser history thanks you.",
            ),
            (
                Quip::Welcome,
                "Ctrl+O to import. We'll call it holiday photos.",
            ),
            (
                Quip::Scanned,
                "Indexed. We didn't touch anything. Can't speak for you.",
            ),
            (Quip::Scanned, "Those \"Linux ISOs\" were not ISOs."),
            (
                Quip::Downloaded,
                "Downloaded. Delivered discreetly, in plain packaging.",
            ),
            (
                Quip::Downloaded,
                "Download complete. It's for the codec, we know.",
            ),
            (
                Quip::EmptyResults,
                "No match. Search forgives accents, not imagination.",
            ),
            (
                Quip::InboxZero,
                "Inbox zero. A productivity win you can tell no one.",
            ),
            (Quip::Buffering, "Loading. Still faster than cargo build."),
            (
                Quip::Starting,
                "Compiling… no, wait, that's just anticipation.",
            ),
        ];
        for (q, en) in new {
            let at = lines(Lang::En, q).iter().position(|s| *s == en);
            let at = at.unwrap_or_else(|| panic!("missing in en: {en}"));
            assert!(en.chars().count() < 80, "{en}");
            for l in i18n::ALL {
                let t = lines(l, q)[at];
                assert!(
                    !t.trim().is_empty() && t.chars().count() < 80,
                    "{}: {en}",
                    l.code()
                );
                if l != Lang::En {
                    assert_ne!(t, en, "{} not translated: {en}", l.code());
                }
            }
        }
    }

    #[test]
    fn nothing_is_said_while_jokes_are_off() {
        set_enabled(false);
        assert!(quip(Quip::Welcome).is_none());
        assert!(steady(Quip::Ended).is_none());
        set_enabled(true);
        assert!(quip(Quip::Welcome).is_some());
        set_enabled(false);
    }
}
