import { createRouter, createWebHistory } from 'vue-router'
import HomeView from '@/views/HomeView.vue'
import NewTaskView from '@/views/NewTaskView.vue'
import TaskView from '@/views/TaskView.vue'
import SettingsView from '@/views/SettingsView.vue'
import ProjectsView from '@/views/ProjectsView.vue'
import DecisionsView from '@/views/DecisionsView.vue'

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'home', component: HomeView },
    { path: '/tasks/new', name: 'new-task', component: NewTaskView },
    { path: '/tasks/:jiraKey', name: 'task', component: TaskView, props: true },
    { path: '/projects', name: 'projects', component: ProjectsView },
    { path: '/decisions', name: 'decisions', component: DecisionsView },
    { path: '/settings', name: 'settings', component: SettingsView },
  ],
})
