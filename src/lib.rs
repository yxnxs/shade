use once_cell::sync::OnceCell;
use std::{path::Path, sync::RwLock};
use thiserror::Error;
use tracing::info;

use xcb::{
    x::{
        Atom, ChangeProperty, ChangeWindowAttributes, CloseDown::RetainPermanent, CreateGc,
        CreatePixmap, Cw, Drawable, Gcontext, GetProperty, InternAtom, KillClient,
        Pixmap, SetCloseDownMode, Window, ATOM_ANY, ATOM_NONE, ATOM_PIXMAP,
    },
    Connection, Xid,
};

#[derive(Error, Debug)]
pub enum Error {
    #[error("Xorg roots iterator did not provided any screens")]
    NoScreenFound,

    #[error("Failed to resetup screens")]
    RescreenFailure,

    #[error("XCB Interal error: {0}")]
    XCBInteral(#[from] xcb::Error),

    #[error("IO Error: {0}")]
    IO(#[from] std::io::Error),

    #[error("Image Error: {0}")]
    Image(#[from] image::error::ImageError),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct BackgroundHandle {
    pub(crate) context: Gcontext,
    pub(crate) background_pixmap: Pixmap,
    pub(crate) connection: Connection,
    pub(crate) root: Window,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub buffer: RwLock<Box<[u8]>>,
}

pub enum ScalingMethod {
    Center,
    Fill,
    Max,
    Scale,
    Tile,
}

pub enum OpenMethod<'a> {
    KeepExisting,
    MakeNew,
    LoadFromFile(ScalingMethod, &'a dyn AsRef<Path>),
}

fn resolve_atom(conn: &Connection, window: Window, atom: Atom) -> xcb::Result<Option<u32>> {
    if atom == ATOM_NONE {
        return Ok(None);
    } else {
        let cookie = conn.send_request(&GetProperty {
            r#type: ATOM_ANY,
            delete: false,
            window,
            property: atom,
            long_offset: 0,
            long_length: 1,
        });

        let property = conn.wait_for_reply(cookie)?;

        // TODO Tracing notice
        // TODO See if this return actually returns or just breaks the function
        if property.r#type() != ATOM_PIXMAP {
            Ok(None)
        } else {
            let id = match property.format() {
                32 => property.value::<u32>()[0],
                8 => u32::from_ne_bytes(property.value::<u8>().try_into().unwrap()),
                _ => unreachable!(),
                // TODO Rare instance of 16 bit is indeed reachable
            };
            Ok(Some(id))
        }
    }
}

fn inner_load(open_method: OpenMethod) -> Result<BackgroundHandle> {
    info!("Connecting to the Xorg Server");
    let (connection, screen_number) = Connection::connect(None).map_err(xcb::Error::from)?;

    let screen = connection
        .get_setup()
        .roots()
        .nth(screen_number as usize)
        .ok_or(Error::NoScreenFound)?;

    let window = screen.root();
    let width = screen.width_in_pixels();
    let height = screen.height_in_pixels();

    // This is paradoxically almost always the same
    let shade_pmap = {
        let pid = connection.generate_id();
        let request = CreatePixmap {
            depth: screen.root_depth(),
            pid,
            width,
            height,
            drawable: Drawable::Window(window),
        };

        connection
            .send_and_check_request(&request)
            .map_err(xcb::Error::from)?;

        pid
    };

    info!("Allocated shade pixmap with id {:?}", shade_pmap);

    let gc = {
        let cid = connection.generate_id();
        let request = CreateGc {
            drawable: Drawable::Pixmap(shade_pmap),
            cid,
            value_list: &[],
        };
        connection.send_request(&request);
        cid
    };

    let mut atom_xrootpmap = {
        let cookie = connection.send_request(&InternAtom {
            name: b"_XROOTPMAP_ID",
            only_if_exists: true,
        });

        connection.wait_for_reply(cookie)?.atom()
    };

    let mut atom_esetroot_pmap = {
        let cookie = connection.send_request(&InternAtom {
            name: b"ESETROOT_PMAP_ID",
            only_if_exists: true,
        });

        connection.wait_for_reply(cookie)?.atom()
    };

    // Resolve the ids of the current pixmaps. If anyone is currently drawing to our beloved
    // screen...
    let xrootid = resolve_atom(&connection, window, atom_xrootpmap)?;
    let esetrootid = resolve_atom(&connection, window, atom_esetroot_pmap)?;

    info!("Foreign pixmaps are X: {xrootid:?} | E: {esetrootid:?}");

    // ... we politely kill them
    match (xrootid, esetrootid) {
        (Some(x), Some(e)) => {
            if x == e {
                connection
                    .send_and_check_request(&KillClient { resource: x })
                    .map_err(xcb::Error::from)?;
            } else {
                connection
                    .send_and_check_request(&KillClient { resource: x })
                    .map_err(xcb::Error::from)?;
                connection
                    .send_and_check_request(&KillClient { resource: e })
                    .map_err(xcb::Error::from)?;
            }
        }
        (Some(x), None) => {
            connection
                .send_and_check_request(&KillClient { resource: x })
                .map_err(xcb::Error::from)?;
        }

        (None, Some(e)) => {
            connection
                .send_and_check_request(&KillClient { resource: e })
                .map_err(xcb::Error::from)?;
        }

        (None, None) => {}
    };

    atom_xrootpmap = {
        let cookie = connection.send_request(&InternAtom {
            name: b"_XROOTPMAP_ID",
            only_if_exists: false,
        });

        connection.wait_for_reply(cookie)?.atom()
    };

    atom_esetroot_pmap = {
        let cookie = connection.send_request(&InternAtom {
            name: b"ESETROOT_PMAP_ID",
            only_if_exists: false,
        });

        connection.wait_for_reply(cookie)?.atom()
    };

    if atom_xrootpmap == ATOM_NONE || atom_esetroot_pmap == ATOM_NONE {
        return Err(Error::RescreenFailure);
    }

    connection
        .send_and_check_request(&ChangeProperty {
            property: atom_xrootpmap,
            mode: xcb::x::PropMode::Replace,
            r#type: ATOM_PIXMAP,
            window,
            data: &[shade_pmap.resource_id()],
        })
        .map_err(xcb::Error::from)?;

    connection
        .send_and_check_request(&ChangeProperty {
            property: atom_esetroot_pmap,
            mode: xcb::x::PropMode::Replace,
            r#type: ATOM_PIXMAP,
            window,
            data: &[shade_pmap.resource_id()],
        })
        .map_err(xcb::Error::from)?;

    // TODO This might not work on multi monitors
    // TODO This also requires the monitor to be cleared
    connection.send_request(&ChangeWindowAttributes {
        window,
        value_list: &[Cw::BackPixmap(shade_pmap)],
    });

    connection
        .send_and_check_request(&SetCloseDownMode {
            mode: RetainPermanent,
        })
        .map_err(xcb::Error::from)?;

    connection.flush().map_err(xcb::Error::from)?;

    let handle = BackgroundHandle {
        connection,
        width,
        height,
        root: window,
        buffer: RwLock::new(vec![0; height as usize * width as usize].into_boxed_slice()),
        background_pixmap: shade_pmap,
        context: gc,
    };

    Ok(handle)
}

pub fn load(options: OpenMethod) -> Result<&'static BackgroundHandle> {
    static HANDLE: OnceCell<BackgroundHandle> = OnceCell::new();
    HANDLE.get_or_try_init(|| inner_load(options))
}

