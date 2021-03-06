use ops;
use core::{Source, PackageIdSpec};
use sources::{PathSource};
use util::{CargoResult, human, Config};

pub fn pkgid(manifest_path: &Path,
             spec: Option<&str>,
             config: &Config) -> CargoResult<PackageIdSpec> {
    let mut source = try!(PathSource::for_path(&manifest_path.dir_path(),
                                               config));
    try!(source.update());
    let package = try!(source.root_package());

    let lockfile = package.root().join("Cargo.lock");
    let source_id = package.package_id().source_id();
    let resolve = match try!(ops::load_lockfile(&lockfile, source_id)) {
        Some(resolve) => resolve,
        None => return Err(human("A Cargo.lock must exist for this command"))
    };

    let pkgid = match spec {
        Some(spec) => try!(resolve.query(spec)),
        None => package.package_id(),
    };
    Ok(PackageIdSpec::from_package_id(pkgid))
}
