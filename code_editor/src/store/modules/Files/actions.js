import {types} from "./mutations";
import VFile, {fileTypes} from "@/models/vFile.model";
import omit from "lodash/omit";
import Fuse from "fuse.js";
import fileStorage from "@/utils/StorageDrivers/IndexedDB"; // Switch storage drivers if needed
import {createFile, deleteFile, getAllFiles, rename, updateFileContent} from "@/api";

export default {
    /**
     * Loads all the files available in the localstorage into the store
     */
    loadFiles: async ({commit, dispatch}) => {

        const {data} = await getAllFiles();
        if (data) {
            const filesObject = data.reduce((result, item) => {
                Object.assign(result, {
                    [item.id]: new VFile({...item, editable: false}),
                });
                return result;
            }, {});
            commit(types.SET_FILES, filesObject);
        }

    },
    /**
     * Creates a new file
     */
    createFile: async ({state, commit, dispatch}, fileDetails) => {
       let parent = state.files[fileDetails.parent];


        /**
         * Show the explorer panel while files are being created
         */
        try {
            dispatch("UI/showExplorerPanel", null, {root: true});
        } catch (error) {
            console.error("Unable to commit active panel to explorer");
            console.error(error);
        }

        const details = fileDetails ? fileDetails : {};
        const file = new VFile({...details, type: fileTypes.FILE});
        let parent_file = state.files[file.parent];
        let  path="";
        if(parent_file){
            if(parent_file.parent_path=="root"){
                path = file.name;
            }else {
                path = parent_file.parent_path+"|"+ file.name;
            }
        }
        commit(types.SET_FILES, {
            ...state.files,
            [file.id]: file,
        });
        let parent_path = path;
        await createFile({...file,parent_path});
        return file;
    },

    moveFile: async ({state, commit, dispatch}, {id, directoryId}) => {
        if (!id) return;

        commit(types.SET_FILES, {
            ...state.files,
            [id]: {
                ...state.files[id],
                parent: directoryId,
                editable: false,
            },
        });
        fileStorage.move({id, parent_id: directoryId});
    },

    createDirectory: async ({state, commit, dispatch}, directoryDetails) => {

        /**
         * Show the explorer panel while directories are being created
         */
        try {
            dispatch("UI/showExplorerPanel", null, {root: true});
        } catch (error) {
            console.error("Unable to commit active panel to explorer");
            console.error(error);
        }

        const details = directoryDetails ? directoryDetails : {};
        const directory = new VFile({...details, type: fileTypes.DIRECTORY});
        commit(types.SET_FILES, {
            ...state.files,
            [directory.id]: directory,
        });
        let parent_path = directory.parent=="root"?"":directory.parent_path;
        await createFile({...directory,parent_path});
    },

    updateFileContents: async ({state, commit, dispatch}, {id, contents}) => {
        if (!id) return;
        let files = state.files[id];
        commit(types.SET_FILES, {
            ...state.files,
            [id]: {
                ...state.files[id],
                contents,
            },
        });
        let parent_path = files.parent=="root"?"":files.parent_path;
        let updateFile ={...files,parent_path:parent_path,contents:contents}
        await updateFileContent(updateFile);
    },
    renameFile: async ({state, commit}, {id, name}) => {
        if (!id) return;
        let file = state.files[id];
        let parent_file = state.files[file.parent];
        let  path="";
        if(parent_file){
            if(parent_file.parent_path=="root"){
                path = parent_file.name;
            }else {
                path = parent_file.parent_path+"|"+ parent_file.name;
            }
        }
        commit(types.SET_FILES, {
            ...state.files,
            [id]: {
                ...state.files[id],
                name,
                editable: false,
            },
        });

        await  rename({
            ...state.files[id],
            bname:file.name,
            cname:name,
            parent_path:path
        });
    },

    openRenameMode: async ({state, commit}, {id}) => {
        if (!id) return;

        commit(types.SET_FILES, {
            ...state.files,
            [id]: {
                ...state.files[id],
                editable: true,
            },
        });
    },

    deleteFile: async ({state, commit, dispatch}, {id}) => {
        if (!id) return;
        let file = state.files[id];
        console.log("Inside delete file")
        await dispatch("Editor/closeFileFromAllEditor", {id}, {root: true});
        commit(types.SET_FILES, omit(state.files, id));
        let  parent_path = file.parent=="root"?"":file.parent_path;
        await deleteFile({...file,cname:file.name,parent_path});
    },
    deleteDirectory: async ({state, commit, dispatch, rootGetters}, {id}) => {
        if (!id) return;

        const children = rootGetters["Editor/getChildren"](id);
        // delete all the children of the directory first
        for (let i = 0; i < children.length; i++) {
            const child = children[i];
            if (child.type === fileTypes.DIRECTORY) {
                await dispatch("deleteDirectory", {id: child.id});
            } else {
                await dispatch("deleteFile", {id: child.id});
            }
        }
        // then delete the directory
        await dispatch("deleteFile", {id});
    },
    searchFiles: async ({state, commit}, {target: {value}}) => {
        const options = {
            includeScore: true,
            threshold: 0.2,
            keys: ["name"],
        };

        const fuse = new Fuse(Object.values(state.files), options);

        const filteredFiles = fuse.search(value).map(({item}) => item);
        commit(types.SET_FILTERED_FILES, filteredFiles);
    },
    createExportPayload: async ({state}) => {
        return {
            files: state.files,
        };
    },
    restoreFiles: async ({state, commit}, {files}) => {
        const newFiles = Object.keys(files).reduce((result, fileId) => {
            return {
                ...result,
                [fileId]: new VFile(files[fileId]),
            };
        }, {});

        const newFilesList = {
            ...state.files,
            ...newFiles,
        };
        commit(types.SET_FILES, newFilesList);


        return true;
    },
};
