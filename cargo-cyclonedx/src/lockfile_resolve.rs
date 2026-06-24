use cargo_lock::package::SourceId;
use cargo_lock::{Dependency as LockDependency, Lockfile, Package as LockPackage};
use cargo_metadata::{
    DependencyKind, Node, NodeDep, Package, PackageId, Source,
};
use std::collections::BTreeMap;

type PackageMap = BTreeMap<PackageId, Package>;
type ResolveMap = BTreeMap<PackageId, Node>;

/// Merges lockfile packages into the metadata package map and builds a resolve graph.
pub fn packages_and_resolve_from_lockfile(
    mut packages: PackageMap,
    lockfile: &Lockfile,
) -> (PackageMap, ResolveMap) {
    merge_lockfile_packages(&mut packages, lockfile);
    let resolve = build_resolve_from_lockfile(&packages, lockfile);
    (packages, resolve)
}

fn merge_lockfile_packages(packages: &mut PackageMap, lockfile: &Lockfile) {
    for lock_pkg in &lockfile.packages {
        if packages
            .values()
            .any(|meta_pkg| packages_match(meta_pkg, lock_pkg))
        {
            continue;
        }
        let pkg = synthetic_package_from_lockfile(lock_pkg);
        packages.insert(pkg.id.clone(), pkg);
    }
}

fn build_resolve_from_lockfile(packages: &PackageMap, lockfile: &Lockfile) -> ResolveMap {
    let id_for_lock_pkg = |lock_pkg: &LockPackage| -> PackageId {
        packages
            .values()
            .find(|meta_pkg| packages_match(meta_pkg, lock_pkg))
            .map(|meta_pkg| meta_pkg.id.clone())
            .unwrap_or_else(|| PackageId {
                repr: lockfile_package_id(lock_pkg),
            })
    };

    lockfile
        .packages
        .iter()
        .map(|lock_pkg| {
            let id = id_for_lock_pkg(lock_pkg);
            let parent = &packages[&id];
            let deps: Vec<NodeDep> = lock_pkg
                .dependencies
                .iter()
                .filter_map(|dep| {
                    let child_lock_pkg = lockfile_package_for_dependency(lockfile, dep)?;
                    let child_id = id_for_lock_pkg(child_lock_pkg);
                    let dep_kind = dep_kind_for_edge(parent, &packages[&child_id]);
                    Some(node_dep(&packages[&child_id].name, &child_id, dep_kind))
                })
                .collect();
            let dependencies: Vec<PackageId> = deps.iter().map(|dep| dep.pkg.clone()).collect();
            let node = resolve_node(&id, &deps, &dependencies);
            (node.id.clone(), node)
        })
        .collect()
}

fn node_dep(name: &str, pkg: &PackageId, kind: DependencyKind) -> NodeDep {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "pkg": pkg.repr,
        "dep_kinds": [{ "kind": kind }],
    }))
    .expect("valid NodeDep JSON")
}

fn resolve_node(id: &PackageId, deps: &[NodeDep], dependencies: &[PackageId]) -> Node {
    serde_json::from_value(serde_json::json!({
        "id": id.repr,
        "deps": deps,
        "dependencies": dependencies.iter().map(|dep| &dep.repr).collect::<Vec<_>>(),
    }))
    .expect("valid Node JSON")
}

fn lockfile_package_for_dependency<'a>(
    lockfile: &'a Lockfile,
    dep: &LockDependency,
) -> Option<&'a LockPackage> {
    lockfile.packages.iter().find(|pkg| {
        pkg.name == dep.name
            && pkg.version == dep.version
            && sources_match_lock(&pkg.source, &dep.source)
    })
}

fn dep_kind_for_edge(parent: &Package, child: &Package) -> DependencyKind {
    for dep in &parent.dependencies {
        let dep_name = dep.rename.as_deref().unwrap_or(dep.name.as_str());
        if dep_name == child.name {
            return dep.kind;
        }
    }
    DependencyKind::Normal
}

fn packages_match(meta_pkg: &Package, lock_pkg: &LockPackage) -> bool {
    meta_pkg.name == lock_pkg.name.as_str()
        && meta_pkg.version == lock_pkg.version
        && sources_match(&meta_pkg.source, &lock_pkg.source)
}

fn sources_match(meta_source: &Option<Source>, lock_source: &Option<SourceId>) -> bool {
    match (meta_source, lock_source) {
        (None, None) => true,
        (Some(meta), Some(lock)) => meta.repr == lock.to_string(),
        _ => false,
    }
}

fn sources_match_lock(a: &Option<SourceId>, b: &Option<SourceId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn synthetic_package_from_lockfile(lock_pkg: &LockPackage) -> Package {
    let mut value = serde_json::json!({
        "name": lock_pkg.name.to_string(),
        "version": lock_pkg.version.to_string(),
        "id": lockfile_package_id(lock_pkg),
        "manifest_path": "Cargo.lock",
        "dependencies": [],
    });
    if let Some(source) = lock_pkg.source.as_ref() {
        value["source"] = serde_json::Value::String(source.to_string());
    }
    serde_json::from_value(value).expect("valid Package JSON")
}

/// Returns a Cargo unique identifier for a lockfile package.
/// See `cargo help pkgid` for more info.
pub fn lockfile_package_id(pkg: &LockPackage) -> String {
    match pkg.source.as_ref() {
        Some(source) => format!("{}#{}@{}", source, pkg.name, pkg.version),
        None => format!("{}@{}", pkg.name, pkg.version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_metadata::MetadataCommand;
    use std::path::PathBuf;

    fn fixture_lockfile() -> Lockfile {
        let mut lock_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        lock_path.push("tests/fixtures/build_then_runtime_dep/Cargo.lock");
        Lockfile::load(lock_path).unwrap()
    }

    fn fixture_metadata_packages() -> PackageMap {
        let mut manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.push("tests/fixtures/build_then_runtime_dep/top_level_crate/Cargo.toml");
        let metadata = MetadataCommand::new()
            .manifest_path(&manifest)
            .no_deps()
            .exec()
            .unwrap();
        metadata
            .packages
            .into_iter()
            .map(|pkg| (pkg.id.clone(), pkg))
            .collect()
    }

    #[test]
    fn lockfile_resolve_includes_all_lockfile_packages() {
        let lockfile = fixture_lockfile();
        let packages = fixture_metadata_packages();
        let (packages, resolve) = packages_and_resolve_from_lockfile(packages, &lockfile);
        assert_eq!(resolve.len(), lockfile.packages.len());
        assert_eq!(packages.len(), lockfile.packages.len());
    }

    #[test]
    fn lockfile_resolve_preserves_build_dependency_kind() {
        let lockfile = fixture_lockfile();
        let packages = fixture_metadata_packages();
        let (_packages, resolve) = packages_and_resolve_from_lockfile(packages, &lockfile);
        let root = resolve
            .keys()
            .find(|id| id.repr.contains("top_level_crate"))
            .unwrap();
        let root_node = &resolve[root];
        assert_eq!(root_node.dependencies.len(), 1);
        let build_dep = &root_node.deps[0];
        assert_eq!(build_dep.dep_kinds[0].kind, DependencyKind::Build);
    }
}
